#!/usr/bin/env bash

set -euo pipefail
export LC_ALL=C

# 输出 RESP2 命令流，交给 redis-cli --pipe 批量导入。
emit_command() {
    printf '*%d\r\n' "$#"
    local argument
    for argument in "$@"; do
        printf '$%d\r\n%s\r\n' "${#argument}" "$argument"
    done
}

emit_command SELECT 0
emit_command FLUSHDB
emit_command SELECT 15
emit_command FLUSHDB
emit_command SELECT 0

for ((i = 1; i <= 30000; i++)); do
    emit_command SET "string:$i" "value-$i"
done

for ((i = 1; i <= 5000; i++)); do
    emit_command HSET "hash:$i" name "user-$i" score "$((i % 100))" active "$((i % 2))"
done

for ((i = 1; i <= 3000; i++)); do
    emit_command RPUSH "list:$i" first "value-$i" last
done

for ((i = 1; i <= 3000; i++)); do
    emit_command SADD "set:$i" alpha "group-$((i % 10))" "member-$i"
done

for ((i = 1; i <= 3000; i++)); do
    emit_command ZADD "zset:$i" 1.5 alpha 2.5 "member-$i"
done

for ((i = 1; i <= 1000; i++)); do
    emit_command XADD "stream:$i" '*' event created sequence "$i"
done

for ((i = 1; i <= 1000; i++)); do
    emit_command SET "ttl:$i" "expires-$i"
    emit_command EXPIRE "ttl:$i" 3600
done
