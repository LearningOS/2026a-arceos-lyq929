#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

// 控制台问候语按 ANSI 转义序列上色，各控制码含义如下：
//   \x1b[1;32m  加粗 + 绿色前景色
//   \x1b[1;36m  加粗 + 青色前景色
//   \x1b[0m     清除全部显示属性，回到终端默认样式
//
// 彩色片段之间的纯文本 "[WithColor]: Hello, Arceos!" 保持完整，
// 这样去掉控制字符后输出依然能被 grep 之类的工具检索到。
#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    println!("\x1b[1;32m[WithColor]\x1b[0m: \x1b[1;36mHello, Arceos!\x1b[0m");
}
