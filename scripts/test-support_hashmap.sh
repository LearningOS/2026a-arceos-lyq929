#!/bin/bash

tmp_file=hashmap_output.txt
grep_content="Memory tests run OK!"

cd "$(dirname "$0")/../arceos" || exit 1


rm pflash.img -f
rm disk.img -f 

make pflash_img
make disk_img


make run A=exercises/support_hashmap/ | tee $tmp_file

output=$(tail -n1 ./$tmp_file | grep -a "$grep_content")

rm -rf $tmp_file 

if [[ -z "$output" ]]; then
    echo "support_hashmap default"
    exit 1
else 
    echo "support_hashmap pass"
    exit 0
fi
