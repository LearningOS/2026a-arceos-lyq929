//! 用开放寻址（线性探测）实现的 `HashMap`。
//!
//! `alloc::collections` 里只有 BTreeMap，没有哈希表，所以这里给 no_std 程序
//! 补一个。实现了应用中最常用的一组接口：`insert`、`get`、`get_mut`、
//! `contains_key`、`remove`、`iter`、`keys`、`values` 等。

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::hash::{BuildHasher, Hash, Hasher};

/// 槽位数的下限，必须是 2 的幂（取索引时直接按位与）。
const MIN_SLOTS: usize = 32;
/// 装载率上限（百分比）。超过就扩容，避免探测链过长导致查询变慢。
const MAX_FILL: usize = 75;

/// FNV-1a 64 位哈希器，末尾再补一步收尾混淆。
pub struct FnvHasher {
    digest: u64,
}

impl Default for FnvHasher {
    fn default() -> Self {
        Self {
            digest: 0xcbf2_9ce4_8422_2325,
        }
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        // 槽位索引只用到低位，这里用 murmur3 的 fmix64 把高位扰动扩散下来。
        let mut x = self.digest;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut x = self.digest;
        for byte in bytes {
            x ^= *byte as u64;
            x = x.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.digest = x;
    }
}

/// [`HashMap`] 默认使用的 [`BuildHasher`]。
#[derive(Default, Clone, Copy, Debug)]
pub struct FnvBuildHasher;

impl BuildHasher for FnvBuildHasher {
    type Hasher = FnvHasher;

    fn build_hasher(&self) -> FnvHasher {
        FnvHasher::default()
    }
}

/// 开放寻址哈希表，键类型需要实现 [`Hash`] 与 [`Eq`]。
pub struct HashMap<K, V, S = FnvBuildHasher> {
    /// 槽位数组，`None` 表示空槽。
    slots: Vec<Option<(K, V)>>,
    /// 当前存放的键值对个数。
    count: usize,
    hasher: S,
}

impl<K, V> HashMap<K, V, FnvBuildHasher>
where
    K: Hash + Eq,
{
    /// 创建一个空表。
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// 创建空表，并预先留出至少 `capacity` 个元素的位置。
    pub fn with_capacity(capacity: usize) -> Self {
        let mut map = Self::with_hasher(FnvBuildHasher);
        if capacity > 0 {
            // 留出装载因子的余量，避免刚插入就扩容。
            let slots = capacity * 100 / MAX_FILL + 1;
            map.resize_slots(slots);
        }
        map
    }
}

impl<K, V> Default for HashMap<K, V, FnvBuildHasher>
where
    K: Hash + Eq,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S> HashMap<K, V, S> {
    /// 用给定的哈希构造器创建空表。
    pub const fn with_hasher(hasher: S) -> Self {
        Self {
            slots: Vec::new(),
            count: 0,
            hasher,
        }
    }

    /// 返回键值对个数。
    pub fn len(&self) -> usize {
        self.count
    }

    /// 表为空时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 不扩容还能再放多少元素。
    pub fn capacity(&self) -> usize {
        self.slots.len() * MAX_FILL / 100
    }

    /// 清空所有元素。
    pub fn clear(&mut self) {
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
        self.count = 0;
    }

    /// 遍历所有键值对。
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            slots: self.slots.iter(),
        }
    }

    /// 遍历所有键。
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }

    /// 遍历所有值。
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
}

impl<K, V, S> HashMap<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    /// 插入一个键值对。
    ///
    /// 键原本不存在时返回 `None`；已存在则覆盖旧值并把旧值返回。
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let slots = self.slots.len();
        if slots == 0 {
            self.resize_slots(MIN_SLOTS);
        } else if (self.count + 1) * 100 > slots * MAX_FILL {
            self.resize_slots(slots * 2);
        }

        // 键已经有了，原地换个值就行。
        if let Some(idx) = self.locate(&key) {
            let slot = self.slots[idx].as_mut().unwrap();
            return Some(core::mem::replace(&mut slot.1, value));
        }

        // 否则顺着探测链找到第一个空槽放进去。
        let mask = self.slots.len() - 1;
        let mut idx = (self.hash_key(&key) as usize) & mask;
        while self.slots[idx].is_some() {
            idx = (idx + 1) & mask;
        }
        self.slots[idx] = Some((key, value));
        self.count += 1;
        None
    }

    /// 取出键对应的值的共享引用。
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.locate(key) {
            Some(idx) => Some(&self.slots[idx].as_ref().unwrap().1),
            None => None,
        }
    }

    /// 取出键对应的值的可变引用。
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.locate(key) {
            Some(idx) => {
                let slot = self.slots[idx].as_mut().unwrap();
                Some(&mut slot.1)
            }
            None => None,
        }
    }

    /// 判断键是否存在。
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.locate(key).is_some()
    }

    /// 删除键并返回曾经存的值。
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let idx = self.locate(key)?;
        let (_, value) = self.slots[idx].take().unwrap();
        self.count -= 1;
        // 挖空一个槽会截断探测链，索性用同样的容量重建一遍表。
        let slots = self.slots.len();
        self.resize_slots(slots);
        Some(value)
    }

    /// 定位键所在的槽位下标，找不到返回 `None`。
    fn locate<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut idx = (self.hash_key(key) as usize) & mask;
        loop {
            match &self.slots[idx] {
                // 遇到空槽说明键不在表里。
                None => return None,
                Some((k, _)) if k.borrow() == key => return Some(idx),
                Some(_) => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    /// 计算键的 64 位哈希值。
    fn hash_key<Q>(&self, key: &Q) -> u64
    where
        Q: Hash + ?Sized,
    {
        let mut hasher = self.hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// 按目标槽位数重建整张表，槽位数会被补齐到 2 的幂。
    fn resize_slots(&mut self, want: usize) {
        let slots_len = want.max(MIN_SLOTS).next_power_of_two();

        let old = core::mem::take(&mut self.slots);
        self.slots = Vec::with_capacity(slots_len);
        self.slots.resize_with(slots_len, || None);

        let mask = slots_len - 1;
        for entry in old {
            if let Some((k, v)) = entry {
                let mut idx = (self.hash_key(&k) as usize) & mask;
                while self.slots[idx].is_some() {
                    idx = (idx + 1) & mask;
                }
                self.slots[idx] = Some((k, v));
            }
        }
    }
}

/// [`HashMap`] 的只读迭代器。
pub struct Iter<'a, K, V> {
    slots: core::slice::Iter<'a, Option<(K, V)>>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        for slot in self.slots.by_ref() {
            if let Some((k, v)) = slot {
                return Some((k, v));
            }
        }
        None
    }
}

impl<'a, K, V> IntoIterator for &'a HashMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
