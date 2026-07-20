\set ON_ERROR_STOP on

DROP VIEW IF EXISTS public.active_recent_records;
DROP TABLE IF EXISTS public.large_values;
DROP TABLE IF EXISTS public.bulk_records;
DROP TABLE IF EXISTS public.type_matrix;
DROP SCHEMA IF EXISTS analytics CASCADE;
DROP TYPE IF EXISTS public.record_state;

CREATE TYPE public.record_state AS ENUM ('new', 'active', 'archived');

CREATE TABLE public.type_matrix (
    id BIGINT PRIMARY KEY,
    small_value SMALLINT NOT NULL,
    integer_value INTEGER NOT NULL,
    bigint_value BIGINT NOT NULL,
    numeric_value NUMERIC(38, 10) NOT NULL,
    real_value REAL NOT NULL,
    double_value DOUBLE PRECISION NOT NULL,
    bool_value BOOLEAN NOT NULL,
    char_value CHAR(16) NOT NULL,
    varchar_value VARCHAR(255) NOT NULL,
    text_value TEXT NOT NULL,
    bytea_value BYTEA NOT NULL,
    date_value DATE NOT NULL,
    time_value TIME(6) NOT NULL,
    timetz_value TIME(6) WITH TIME ZONE NOT NULL,
    timestamp_value TIMESTAMP(6) NOT NULL,
    timestamptz_value TIMESTAMP(6) WITH TIME ZONE NOT NULL,
    interval_value INTERVAL NOT NULL,
    uuid_value UUID NOT NULL,
    json_value JSON NOT NULL,
    jsonb_value JSONB NOT NULL,
    int_array INTEGER[] NOT NULL,
    text_array TEXT[] NOT NULL,
    range_value INT4RANGE NOT NULL,
    inet_value INET NOT NULL,
    cidr_value CIDR NOT NULL,
    mac_value MACADDR NOT NULL,
    bit_value BIT(8) NOT NULL,
    varbit_value VARBIT NOT NULL,
    xml_value XML NOT NULL,
    search_value TSVECTOR NOT NULL,
    state_value public.record_state NOT NULL,
    nullable_value TEXT NULL
);

INSERT INTO public.type_matrix VALUES
    (
        1, 32767, 2147483647, 9223372036854775807,
        9999999999999999999999999999.9999999999, 12345.125,
        1.7976931348623157e100, TRUE, 'ascii', 'Ramag database client',
        E'line 1\nline 2', decode('000102ff', 'hex'), '2024-02-29',
        '23:59:59.999999', '23:59:59.999999+08', '2030-12-31 23:59:59.999999',
        '2030-12-31 23:59:59.999999+08', '1 year 2 mons 3 days 04:05:06.000007',
        '11111111-1111-1111-1111-111111111111',
        '{"kind":"json","items":[1,"二",null]}',
        '{"kind":"jsonb","nested":{"enabled":true}}',
        ARRAY[1, 2, NULL], ARRAY['alpha', '中文', NULL], '[1,10)',
        '2001:db8::1/64', '10.0.0.0/8', '08:00:2b:01:02:03',
        B'10101010', B'10101', '<root><item>数据</item></root>',
        to_tsvector('simple', 'ramag database client'), 'active', NULL
    ),
    (
        2, -32768, -2147483648, -9223372036854775807,
        -9999999999999999999999999999.9999999999, -0.5,
        -1.25e-100, FALSE, '中文', '', '', decode('ff00fe', 'hex'),
        '1970-01-01', '00:00:00.000001', '00:00:00.000001-05',
        '1970-01-01 00:00:00.000001', '2001-01-01 00:00:00.000001+00',
        '-2 days 03:04:05', '22222222-2222-2222-2222-222222222222',
        '[true,false,null]', '{"decimal":"123.4500"}',
        ARRAY[-1, 0, 1], ARRAY['', 'emoji 🚀'], 'empty',
        '192.168.1.25', '2001:db8::/32', 'ff:ff:ff:ff:ff:ff',
        B'00000001', B'1', '<empty/>', to_tsvector('simple', ''),
        'archived', 'optional text'
    ),
    (
        3, 0, 0, 0, 0.0000000000, 0.0, 0.0, TRUE,
        'padding', 'quotes '' and "', '边界值', ''::bytea, '2000-01-01',
        '12:34:56.123456', '12:34:56.123456+00', '2000-01-01 12:34:56.123456',
        '2000-01-01 12:34:56.123456+00', '0 seconds',
        '33333333-3333-3333-3333-333333333333', '{}', '{}',
        ARRAY[]::INTEGER[], ARRAY[]::TEXT[], '[5,5]', '127.0.0.1',
        '127.0.0.0/8', '00:00:00:00:00:00', B'11110000', B'',
        '<root/>', to_tsvector('simple', 'boundary'), 'new', NULL
    );

CREATE TABLE public.bulk_records (
    id BIGINT PRIMARY KEY,
    group_id INTEGER NOT NULL,
    status public.record_state NOT NULL,
    amount NUMERIC(20, 4) NOT NULL,
    title VARCHAR(255) NOT NULL,
    body TEXT NOT NULL,
    payload JSONB NOT NULL,
    tags TEXT[] NOT NULL,
    binary_token BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

INSERT INTO public.bulk_records
SELECT
    sequence_id,
    sequence_id % 1000,
    (ARRAY['active', 'new', 'archived']::public.record_state[])[(sequence_id % 3) + 1],
    (sequence_id * 1.2345) - 50000,
    'record-' || lpad(sequence_id::TEXT, 6, '0'),
    'Bulk row ' || sequence_id || '：中文、日本語、한국어、emoji 🙂',
    jsonb_build_object(
        'id', sequence_id,
        'group', sequence_id % 1000,
        'flags', jsonb_build_array(sequence_id % 2 = 0, sequence_id % 5 = 0),
        'nullable', CASE WHEN sequence_id % 11 = 0 THEN NULL ELSE to_jsonb('value-' || sequence_id) END
    ),
    ARRAY['group-' || (sequence_id % 10), 'bucket-' || (sequence_id % 100)],
    decode(md5('ramag-' || sequence_id), 'hex'),
    TIMESTAMPTZ '2020-01-01 00:00:00+00' + make_interval(secs => sequence_id % 2000000),
    TIMESTAMPTZ '2024-01-01 00:00:00+00' + make_interval(secs => sequence_id % 1000000)
FROM generate_series(1, 100000) AS sequence_id;

CREATE INDEX idx_bulk_group_created ON public.bulk_records (group_id, created_at);
CREATE INDEX idx_bulk_status_amount ON public.bulk_records (status, amount);
CREATE INDEX idx_bulk_payload ON public.bulk_records USING GIN (payload);
CREATE INDEX idx_bulk_tags ON public.bulk_records USING GIN (tags);

CREATE TABLE public.large_values (
    id BIGINT PRIMARY KEY,
    text_value TEXT NOT NULL,
    bytea_value BYTEA NOT NULL,
    jsonb_value JSONB NOT NULL
);

INSERT INTO public.large_values VALUES (
    1,
    repeat('Ramag数据库类型测试-', 80000),
    decode(repeat('ab', 1048576), 'hex'),
    jsonb_build_object(
        'description', repeat('large-json-多语言-', 40000),
        'metadata', jsonb_build_object('source', 'db-test', 'complete', TRUE)
    )
);

CREATE VIEW public.active_recent_records AS
SELECT id, group_id, amount, title, created_at
FROM public.bulk_records
WHERE status = 'active' AND id % 2 = 0;

CREATE SCHEMA analytics;

CREATE TABLE analytics.daily_metrics (
    sample_id BIGINT PRIMARY KEY,
    metric_date DATE NOT NULL,
    region TEXT NOT NULL,
    service TEXT NOT NULL,
    request_count BIGINT NOT NULL,
    error_rate NUMERIC(8, 6) NOT NULL,
    latency_ms DOUBLE PRECISION NOT NULL,
    dimensions JSONB NOT NULL
);

INSERT INTO analytics.daily_metrics
SELECT
    sequence_id,
    DATE '2024-01-01' + (sequence_id % 400),
    'region-' || (sequence_id % 10),
    'service-' || (sequence_id % 20),
    sequence_id * 17,
    (sequence_id % 1000)::NUMERIC / 100000,
    1.5 + (sequence_id % 500) * 0.25,
    jsonb_build_object('sample', sequence_id, 'healthy', sequence_id % 7 <> 0)
FROM generate_series(1, 8000) AS sequence_id;

CREATE INDEX idx_daily_metrics_dimensions
ON analytics.daily_metrics (metric_date, region, service);

SELECT 'type_matrix' AS dataset, COUNT(*) AS row_count FROM public.type_matrix
UNION ALL
SELECT 'bulk_records', COUNT(*) FROM public.bulk_records
UNION ALL
SELECT 'large_values', COUNT(*) FROM public.large_values
UNION ALL
SELECT 'daily_metrics', COUNT(*) FROM analytics.daily_metrics;
