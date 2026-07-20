-- 补充二进制、特殊结构与超大集合；基础 key 由 RESP 管道高效导入。
redis.call('SET', 'special:unicode', '中文、日本語、한국어、emoji 🙂')
redis.call('SET', 'special:json', '{"kind":"json","nested":{"enabled":true}}')
redis.call('SET', 'special:integer', '9223372036854775807')
redis.call('SET', 'special:float', '3.141592653589793')
redis.call('SET', 'special:binary', string.char(0, 1, 2, 127, 128, 255))
redis.call('SETBIT', 'special:bitmap', 1, 1)
redis.call('SETBIT', 'special:bitmap', 8191, 1)
redis.call('PFADD', 'special:hll', 'alpha', 'beta', 'gamma', 'delta')
redis.call('GEOADD', 'special:geo', 121.4737, 31.2304, 'shanghai')
redis.call('GEOADD', 'special:geo', 116.4074, 39.9042, 'beijing')

redis.call('SET', 'large:string', string.rep('x', 8 * 1024 * 1024))

for i = 1, 20000 do
    redis.call('HSET', 'large:hash', 'field:' .. i, 'value:' .. i)
    redis.call('RPUSH', 'large:list', 'item:' .. i)
    redis.call('SADD', 'large:set', 'member:' .. i)
    redis.call('ZADD', 'large:zset', i / 10, 'member:' .. i)
end

for i = 1, 10000 do
    redis.call('XADD', 'large:stream', '*', 'event', 'metric', 'sequence', i)
end

return redis.call('DBSIZE')
