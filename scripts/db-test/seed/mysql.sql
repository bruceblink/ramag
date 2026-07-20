SET NAMES utf8mb4;
SET FOREIGN_KEY_CHECKS = 0;

DROP VIEW IF EXISTS active_recent_records;
DROP TABLE IF EXISTS spatial_samples;
DROP TABLE IF EXISTS large_values;
DROP TABLE IF EXISTS bulk_records;
DROP TABLE IF EXISTS type_matrix;
DROP TABLE IF EXISTS seed_digits;

SET FOREIGN_KEY_CHECKS = 1;

CREATE TABLE type_matrix (
    id BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    tiny_signed TINYINT NOT NULL,
    tiny_unsigned TINYINT UNSIGNED NOT NULL,
    small_signed SMALLINT NOT NULL,
    small_unsigned SMALLINT UNSIGNED NOT NULL,
    medium_signed MEDIUMINT NOT NULL,
    medium_unsigned MEDIUMINT UNSIGNED NOT NULL,
    integer_signed INT NOT NULL,
    integer_unsigned INT UNSIGNED NOT NULL,
    bigint_signed BIGINT NOT NULL,
    bigint_unsigned BIGINT UNSIGNED NOT NULL,
    decimal_value DECIMAL(38, 10) NOT NULL,
    float_value FLOAT NOT NULL,
    double_value DOUBLE NOT NULL,
    boolean_value BOOLEAN NOT NULL,
    char_value CHAR(16) NOT NULL,
    varchar_value VARCHAR(255) NOT NULL,
    text_value TEXT NOT NULL,
    medium_text_value MEDIUMTEXT NOT NULL,
    binary_value BINARY(8) NOT NULL,
    varbinary_value VARBINARY(64) NOT NULL,
    blob_value BLOB NOT NULL,
    bit_value BIT(16) NOT NULL,
    date_value DATE NOT NULL,
    time_value TIME(6) NOT NULL,
    datetime_value DATETIME(6) NOT NULL,
    timestamp_value TIMESTAMP(6) NULL,
    year_value YEAR NOT NULL,
    json_value JSON NOT NULL,
    enum_value ENUM('new', 'active', 'archived') NOT NULL,
    set_value SET('alpha', 'beta', 'gamma') NOT NULL,
    nullable_value VARCHAR(255) NULL
) ENGINE = InnoDB;

INSERT INTO type_matrix VALUES
    (
        1, 127, 255, 32767, 65535, 8388607, 16777215,
        2147483647, 4294967295, 9223372036854775807,
        18446744073709551615, 9999999999999999999999999999.9999999999,
        12345.125, 1.7976931348623157e100, TRUE, 'ascii',
        'Ramag database client', 'line 1\nline 2', '多语言文本：中文、日本語、한국어、🙂',
        X'0001020304050607', X'00FF10AABBCC', X'000102FF', b'1010101011110000',
        '2024-02-29', '23:59:59.999999', '2030-12-31 23:59:59.999999',
        '2030-12-31 23:59:59.999999', 2030,
        JSON_OBJECT('kind', 'object', 'nested', JSON_OBJECT('enabled', TRUE), 'items', JSON_ARRAY(1, '二', NULL)),
        'active', 'alpha,gamma', NULL
    ),
    (
        2, -128, 0, -32768, 0, -8388608, 0,
        -2147483648, 0, -9223372036854775807,
        0, -9999999999999999999999999999.9999999999,
        -0.5, -1.25e-100, FALSE, '中文',
        '', '', 'emoji: 🚀\ncontrol-like text is preserved',
        X'FFFFFFFFFFFFFFFF', X'', X'FF00FE', b'0000000000000001',
        '1970-01-01', '00:00:00.000001', '1970-01-01 00:00:00.000001',
        '2001-01-01 00:00:00.000001', 1970,
        JSON_ARRAY(TRUE, FALSE, NULL, JSON_OBJECT('decimal', '123.4500')),
        'archived', 'beta', 'optional text'
    ),
    (
        3, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
        0.0000000000, 0.0, 0.0, TRUE, 'padding',
        'quotes '' and "', 'tab\tvalue', '边界值',
        X'52414D4147000000', X'52414D4147', X'', b'1111111100000000',
        '2000-01-01', '12:34:56.123456', '2000-01-01 12:34:56.123456',
        NULL, 2000, JSON_OBJECT(), 'new', '', NULL
    );

CREATE TABLE bulk_records (
    id BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    group_id INT UNSIGNED NOT NULL,
    status ENUM('active', 'pending', 'archived') NOT NULL,
    amount DECIMAL(20, 4) NOT NULL,
    title VARCHAR(255) NOT NULL,
    body TEXT NOT NULL,
    payload JSON NOT NULL,
    binary_token BINARY(16) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    updated_at TIMESTAMP(6) NOT NULL,
    INDEX idx_group_created (group_id, created_at),
    INDEX idx_status_amount (status, amount),
    INDEX idx_updated_at (updated_at)
) ENGINE = InnoDB;

CREATE TABLE seed_digits (n TINYINT UNSIGNED NOT NULL PRIMARY KEY);
INSERT INTO seed_digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);

INSERT INTO bulk_records (
    id, group_id, status, amount, title, body, payload, binary_token, created_at, updated_at
)
SELECT
    sequence_id,
    MOD(sequence_id, 1000),
    ELT(MOD(sequence_id, 3) + 1, 'active', 'pending', 'archived'),
    (sequence_id * 1.2345) - 50000,
    CONCAT('record-', LPAD(sequence_id, 6, '0')),
    CONCAT('Bulk row ', sequence_id, '：中文、日本語、한국어、emoji 🙂'),
    JSON_OBJECT(
        'id', sequence_id,
        'group', MOD(sequence_id, 1000),
        'flags', JSON_ARRAY(MOD(sequence_id, 2) = 0, MOD(sequence_id, 5) = 0),
        'nullable', IF(MOD(sequence_id, 11) = 0, NULL, CONCAT('value-', sequence_id))
    ),
    UNHEX(MD5(CONCAT('ramag-', sequence_id))),
    TIMESTAMP('2020-01-01 00:00:00') + INTERVAL MOD(sequence_id, 2000000) SECOND,
    TIMESTAMP('2024-01-01 00:00:00') + INTERVAL MOD(sequence_id, 1000000) SECOND
FROM (
    SELECT
        ones.n
        + tens.n * 10
        + hundreds.n * 100
        + thousands.n * 1000
        + ten_thousands.n * 10000
        + 1 AS sequence_id
    FROM seed_digits AS ones
    CROSS JOIN seed_digits AS tens
    CROSS JOIN seed_digits AS hundreds
    CROSS JOIN seed_digits AS thousands
    CROSS JOIN seed_digits AS ten_thousands
) AS sequence_source;

DROP TABLE seed_digits;

CREATE TABLE large_values (
    id BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    text_value MEDIUMTEXT NOT NULL,
    blob_value MEDIUMBLOB NOT NULL,
    json_value JSON NOT NULL
) ENGINE = InnoDB;

INSERT INTO large_values VALUES (
    1,
    REPEAT('Ramag数据库类型测试-', 80000),
    UNHEX(REPEAT('ab', 1048576)),
    JSON_OBJECT(
        'description', REPEAT('large-json-多语言-', 40000),
        'metadata', JSON_OBJECT('source', 'db-test', 'complete', TRUE)
    )
);

CREATE TABLE spatial_samples (
    id BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    name VARCHAR(64) NOT NULL,
    location POINT NOT NULL SRID 4326,
    area POLYGON NOT NULL SRID 4326,
    SPATIAL INDEX idx_location (location),
    SPATIAL INDEX idx_area (area)
) ENGINE = InnoDB;

INSERT INTO spatial_samples VALUES (
    1,
    'Shanghai sample',
    ST_GeomFromText('POINT(121.4737 31.2304)', 4326, 'axis-order=long-lat'),
    ST_GeomFromText(
        'POLYGON((121.40 31.20,121.55 31.20,121.55 31.30,121.40 31.30,121.40 31.20))',
        4326,
        'axis-order=long-lat'
    )
);

CREATE VIEW active_recent_records AS
SELECT id, group_id, amount, title, created_at
FROM bulk_records
WHERE status = 'active' AND id % 2 = 0;

SELECT 'type_matrix' AS dataset, COUNT(*) AS row_count FROM type_matrix
UNION ALL
SELECT 'bulk_records', COUNT(*) FROM bulk_records
UNION ALL
SELECT 'large_values', COUNT(*) FROM large_values
UNION ALL
SELECT 'spatial_samples', COUNT(*) FROM spatial_samples;
