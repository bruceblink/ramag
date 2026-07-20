/* global ObjectId, NumberDecimal, NumberInt, NumberLong, Timestamp, BinData, MinKey, MaxKey */

// 每批写入固定数量，兼顾速度与 mongosh 内存占用。
function insertBatches(collection, total, factory, batchSize = 1000) {
    for (let start = 0; start < total; start += batchSize) {
        const end = Math.min(start + batchSize, total);
        const documents = [];
        for (let index = start; index < end; index += 1) {
            documents.push(factory(index));
        }
        collection.insertMany(documents, { ordered: false });
    }
}

function deterministicObjectId(sequence) {
    return new ObjectId(sequence.toString(16).padStart(24, '0'));
}

function assertCount(collection, expected) {
    const actual = collection.countDocuments({});
    if (actual !== expected) {
        throw new Error(`${collection.getName()} count mismatch: expected=${expected}, actual=${actual}`);
    }
}

const databaseName = process.env.RAMAG_DB_TEST_MONGO_DATABASE;
if (!databaseName) {
    throw new Error('RAMAG_DB_TEST_MONGO_DATABASE is required');
}

const targetDb = db.getSiblingDB(databaseName);
targetDb.dropDatabase();

const roles = ['admin', 'editor', 'viewer', 'auditor'];
const regions = ['cn-east', 'cn-north', 'ap-south', 'eu-west'];
insertBatches(targetDb.users, 20000, (index) => ({
    _id: deterministicObjectId(index + 1),
    email: `user-${index + 1}@example.test`,
    name: `User ${index + 1}`,
    age: 18 + (index % 63),
    role: roles[index % roles.length],
    active: index % 7 !== 0,
    score: NumberDecimal(((index % 10000) / 100).toFixed(2)),
    tags: [`group-${index % 20}`, `bucket-${index % 100}`],
    profile: {
        region: regions[index % regions.length],
        locale: index % 2 === 0 ? 'zh-CN' : 'en-US',
        bio: `多语言简介 ${index + 1} 🙂`,
    },
    createdAt: new Date(Date.UTC(2020, 0, 1) + index * 60000),
    lastLoginAt: index % 11 === 0 ? null : new Date(Date.UTC(2025, 0, 1) + index * 1000),
}));

const categories = ['electronics', 'books', 'home', 'sports', 'software'];
insertBatches(targetDb.products, 15000, (index) => ({
    _id: deterministicObjectId(100000 + index),
    sku: `SKU-${String(index + 1).padStart(6, '0')}`,
    name: `Product ${index + 1}`,
    category: categories[index % categories.length],
    price: NumberDecimal((9.99 + (index % 5000) / 10).toFixed(2)),
    inventory: NumberInt(index % 1000),
    enabled: index % 13 !== 0,
    dimensions: { width: index % 100, height: index % 80, unit: 'cm' },
    attributes: { color: `color-${index % 12}`, material: `material-${index % 8}` },
    releasedAt: new Date(Date.UTC(2021, 0, 1) + index * 3600000),
}));

const orderStates = ['created', 'paid', 'shipped', 'completed', 'cancelled'];
insertBatches(targetDb.orders, 60000, (index) => {
    const firstProduct = index % 15000;
    const secondProduct = (index + 17) % 15000;
    return {
        _id: deterministicObjectId(200000 + index),
        orderNo: `ORD-${String(index + 1).padStart(8, '0')}`,
        userId: deterministicObjectId((index % 20000) + 1),
        status: orderStates[index % orderStates.length],
        total: NumberDecimal((19.99 + (index % 10000) / 20).toFixed(2)),
        currency: index % 10 === 0 ? 'USD' : 'CNY',
        items: [
            { productId: deterministicObjectId(100000 + firstProduct), quantity: 1 + (index % 3) },
            { productId: deterministicObjectId(100000 + secondProduct), quantity: 1 },
        ],
        shipping: {
            city: index % 2 === 0 ? '上海' : '北京',
            postalCode: String(200000 + (index % 10000)),
        },
        createdAt: new Date(Date.UTC(2023, 0, 1) + index * 30000),
        paidAt: index % 5 === 0 ? null : new Date(Date.UTC(2023, 0, 1) + index * 30000 + 5000),
    };
});

targetDb.type_matrix.insertMany([
    {
        _id: 1,
        stringValue: '中文、日本語、한국어、emoji 🙂',
        int32Value: NumberInt(2147483647),
        int64Value: NumberLong('9223372036854775807'),
        decimalValue: NumberDecimal('999999999999999999999999.9999999999'),
        doubleValue: 3.141592653589793,
        boolValue: true,
        dateValue: new Date('2030-12-31T23:59:59.999Z'),
        timestampValue: Timestamp(1710000000, 1),
        objectIdValue: new ObjectId('64b000000000000000000001'),
        binaryValue: BinData(0, 'AAECA/8='),
        regexValue: /^ramag-[0-9]+$/i,
        arrayValue: [1, 'two', null, { nested: true }],
        objectValue: { level1: { level2: { value: 'deep' } } },
        nullValue: null,
        minKeyValue: MinKey(),
        maxKeyValue: MaxKey(),
    },
    {
        _id: 'string-id',
        stringValue: '',
        int32Value: NumberInt(-2147483648),
        int64Value: NumberLong('-9223372036854775807'),
        decimalValue: NumberDecimal('-0.0000000001'),
        doubleValue: -1.25e-100,
        boolValue: false,
        dateValue: new Date('1970-01-01T00:00:00.000Z'),
        binaryValue: BinData(0, '/wCA'),
        arrayValue: [],
        objectValue: {},
        nullValue: null,
    },
]);

const standardPayload = 'Ramag数据库类型测试-'.repeat(4096);
const oversizedPayload = 'Ramag大型文档-'.repeat(100000);
insertBatches(targetDb.large_documents, 100, (index) => ({
    _id: index,
    name: `large-document-${index}`,
    payload: index === 0 ? oversizedPayload : standardPayload,
    checksumHint: `deterministic-${index}`,
    metadata: { index, generated: true, nullable: index % 10 === 0 ? null : 'value' },
}), 20);

targetDb.createCollection('metrics', {
    timeseries: {
        timeField: 'observedAt',
        metaField: 'metadata',
        granularity: 'minutes',
    },
});
insertBatches(targetDb.metrics, 20000, (index) => ({
    observedAt: new Date(Date.UTC(2025, 0, 1) + index * 60000),
    metadata: { sensor: `sensor-${index % 100}`, region: regions[index % regions.length] },
    temperature: 10 + (index % 400) / 10,
    humidity: 20 + (index % 700) / 10,
    labels: [`floor-${index % 20}`, `zone-${index % 8}`],
}));

targetDb.createCollection('capped_events', { capped: true, size: 16 * 1024 * 1024, max: 10000 });
insertBatches(targetDb.capped_events, 10000, (index) => ({
    sequence: index + 1,
    level: index % 20 === 0 ? 'warn' : 'info',
    message: `Event ${index + 1}: 本地数据库测试`,
    createdAt: new Date(Date.UTC(2026, 0, 1) + index * 1000),
}));

targetDb.users.createIndex({ age: 1 }, { name: 'idx_age' });
targetDb.users.createIndex({ email: 1 }, { name: 'idx_email_uniq', unique: true });
targetDb.users.createIndex({ role: 1, active: 1 }, { name: 'idx_role_active' });
targetDb.products.createIndex({ category: 1, price: 1 }, { name: 'idx_category_price' });
targetDb.products.createIndex({ sku: 1 }, { name: 'idx_sku_uniq', unique: true });
targetDb.orders.createIndex({ userId: 1, createdAt: -1 }, { name: 'idx_user_created' });
targetDb.orders.createIndex({ status: 1, createdAt: -1 }, { name: 'idx_status_created' });

assertCount(targetDb.users, 20000);
assertCount(targetDb.products, 15000);
assertCount(targetDb.orders, 60000);
assertCount(targetDb.type_matrix, 2);
assertCount(targetDb.large_documents, 100);
assertCount(targetDb.metrics, 20000);
assertCount(targetDb.capped_events, 10000);

print(`MongoDB seed complete: database=${databaseName}, documents=125102`);
