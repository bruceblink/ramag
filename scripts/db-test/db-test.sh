#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/compose.yaml"
SEED_DIR="$SCRIPT_DIR/seed"
ENV_FILE="${RAMAG_DB_TEST_ENV_FILE:-$REPO_DIR/.ramag/db-test.env}"
PROJECT_NAME="ramag-db-test"
# 仅用于绑定 127.0.0.1 的本地测试容器，不得复用到其他环境。
FIXED_TEST_PASSWORD="ramag_test_only"
FIXED_MYSQL_ROOT_PASSWORD="ramag_root_test_only"

log() {
    printf '[db-test] %s\n' "$*"
}

fail() {
    printf '[db-test] ERROR: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

managed_volumes_exist() {
    local volume
    for volume in \
        ramag-db-test-mysql-data \
        ramag-db-test-postgres-data \
        ramag-db-test-redis-data \
        ramag-db-test-mongo-data; do
        if docker volume inspect "$volume" >/dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

environment_uses_fixed_credentials() {
    (
        # shellcheck disable=SC1090
        source "$ENV_FILE"
        [[ "${RAMAG_DB_TEST_MYSQL_PASSWORD:-}" == "$FIXED_TEST_PASSWORD" ]] \
            && [[ "${RAMAG_DB_TEST_MYSQL_ROOT_PASSWORD:-}" == "$FIXED_MYSQL_ROOT_PASSWORD" ]] \
            && [[ "${RAMAG_DB_TEST_POSTGRES_PASSWORD:-}" == "$FIXED_TEST_PASSWORD" ]] \
            && [[ "${RAMAG_DB_TEST_REDIS_PASSWORD:-}" == "$FIXED_TEST_PASSWORD" ]] \
            && [[ "${RAMAG_DB_TEST_MONGO_PASSWORD:-}" == "$FIXED_TEST_PASSWORD" ]]
    )
}

read_environment_value() {
    local wanted="$1"
    local name value
    while IFS='=' read -r name value; do
        if [[ "$name" == "$wanted" ]]; then
            printf '%s' "$value"
            return
        fi
    done <"$ENV_FILE"
}

# 默认凭据固定，确保本地连接配置可重复使用；显式指定环境文件时尊重调用方配置。
create_environment() {
    local allow_existing_volumes="${1:-false}"
    if [[ -n "${RAMAG_DB_TEST_ENV_FILE:-}" ]]; then
        [[ -f "$ENV_FILE" ]] || fail "Custom environment file does not exist: $ENV_FILE"
        chmod 600 "$ENV_FILE"
        return
    fi
    if [[ -f "$ENV_FILE" ]] && environment_uses_fixed_credentials; then
        chmod 600 "$ENV_FILE"
        return
    fi
    if [[ -f "$ENV_FILE" ]] \
        && [[ "$allow_existing_volumes" != "true" ]] \
        && managed_volumes_exist; then
        fail "Existing db-test volumes use legacy random credentials. Run 'make db-test-clean' once, then retry."
    fi

    local mysql_port="${RAMAG_DB_TEST_MYSQL_PORT:-}"
    local postgres_port="${RAMAG_DB_TEST_POSTGRES_PORT:-}"
    local redis_port="${RAMAG_DB_TEST_REDIS_PORT:-}"
    local mongo_port="${RAMAG_DB_TEST_MONGO_PORT:-}"
    if [[ -f "$ENV_FILE" ]]; then
        mysql_port="${mysql_port:-$(read_environment_value RAMAG_DB_TEST_MYSQL_PORT)}"
        postgres_port="${postgres_port:-$(read_environment_value RAMAG_DB_TEST_POSTGRES_PORT)}"
        redis_port="${redis_port:-$(read_environment_value RAMAG_DB_TEST_REDIS_PORT)}"
        mongo_port="${mongo_port:-$(read_environment_value RAMAG_DB_TEST_MONGO_PORT)}"
    fi

    mkdir -p "$(dirname "$ENV_FILE")"
    umask 077
    {
        printf 'RAMAG_DB_TEST_MYSQL_PORT=%s\n' "${mysql_port:-13306}"
        printf 'RAMAG_DB_TEST_MYSQL_DATABASE=ramag_test\n'
        printf 'RAMAG_DB_TEST_MYSQL_USER=ramag\n'
        printf 'RAMAG_DB_TEST_MYSQL_PASSWORD=%s\n' "$FIXED_TEST_PASSWORD"
        printf 'RAMAG_DB_TEST_MYSQL_ROOT_PASSWORD=%s\n' "$FIXED_MYSQL_ROOT_PASSWORD"
        printf 'RAMAG_DB_TEST_POSTGRES_PORT=%s\n' "${postgres_port:-15432}"
        printf 'RAMAG_DB_TEST_POSTGRES_DATABASE=ramag_test\n'
        printf 'RAMAG_DB_TEST_POSTGRES_USER=ramag\n'
        printf 'RAMAG_DB_TEST_POSTGRES_PASSWORD=%s\n' "$FIXED_TEST_PASSWORD"
        printf 'RAMAG_DB_TEST_REDIS_PORT=%s\n' "${redis_port:-16379}"
        printf 'RAMAG_DB_TEST_REDIS_PASSWORD=%s\n' "$FIXED_TEST_PASSWORD"
        printf 'RAMAG_DB_TEST_MONGO_PORT=%s\n' "${mongo_port:-27018}"
        printf 'RAMAG_DB_TEST_MONGO_DATABASE=ramag_demo\n'
        printf 'RAMAG_DB_TEST_MONGO_USER=ramag\n'
        printf 'RAMAG_DB_TEST_MONGO_PASSWORD=%s\n' "$FIXED_TEST_PASSWORD"
    } >"$ENV_FILE"
    chmod 600 "$ENV_FILE"
    log "Prepared fixed local test credentials at .ramag/db-test.env"
}

load_environment() {
    # shellcheck disable=SC1090
    set -a
    source "$ENV_FILE"
    set +a

    local required=(
        RAMAG_DB_TEST_MYSQL_PORT
        RAMAG_DB_TEST_MYSQL_DATABASE
        RAMAG_DB_TEST_MYSQL_USER
        RAMAG_DB_TEST_MYSQL_PASSWORD
        RAMAG_DB_TEST_MYSQL_ROOT_PASSWORD
        RAMAG_DB_TEST_POSTGRES_PORT
        RAMAG_DB_TEST_POSTGRES_DATABASE
        RAMAG_DB_TEST_POSTGRES_USER
        RAMAG_DB_TEST_POSTGRES_PASSWORD
        RAMAG_DB_TEST_REDIS_PORT
        RAMAG_DB_TEST_REDIS_PASSWORD
        RAMAG_DB_TEST_MONGO_PORT
        RAMAG_DB_TEST_MONGO_DATABASE
        RAMAG_DB_TEST_MONGO_USER
        RAMAG_DB_TEST_MONGO_PASSWORD
    )
    local name
    for name in "${required[@]}"; do
        [[ -n "${!name:-}" ]] || fail "Missing value in $ENV_FILE: $name"
    done
}

prepare() {
    local allow_existing_volumes="${1:-false}"
    require_command docker
    docker info >/dev/null 2>&1 || fail "Docker daemon is not available"
    docker compose version >/dev/null 2>&1 || fail "Docker Compose is not available"
    create_environment "$allow_existing_volumes"
    load_environment
}

compose() {
    docker compose \
        --project-name "$PROJECT_NAME" \
        --env-file "$ENV_FILE" \
        --file "$COMPOSE_FILE" \
        "$@"
}

start_databases() {
    prepare
    log "Starting MySQL, PostgreSQL, Redis, and MongoDB"
    if ! compose up --detach --wait --wait-timeout 180; then
        compose ps || true
        compose logs --tail=80 || true
        fail "Database startup failed. Check whether ports 13306, 15432, 16379, or 27018 are already in use."
    fi
    log "All database health checks passed"
}

seed_mysql() {
    log "Resetting and seeding MySQL"
    compose exec --no-TTY mysql sh -ec '
        export MYSQL_PWD="$MYSQL_PASSWORD"
        exec mysql --protocol=TCP --host=127.0.0.1 --user="$MYSQL_USER" --default-character-set=utf8mb4 "$MYSQL_DATABASE"
    ' <"$SEED_DIR/mysql.sql"
}

seed_postgres() {
    log "Resetting and seeding PostgreSQL"
    compose exec --no-TTY postgres sh -ec '
        export PGPASSWORD="$POSTGRES_PASSWORD"
        exec psql --host=127.0.0.1 --username="$POSTGRES_USER" --dbname="$POSTGRES_DB"
    ' <"$SEED_DIR/postgres.sql"
}

seed_redis() {
    log "Resetting and seeding Redis databases 0 and 15"
    "$SEED_DIR/redis-protocol.sh" | compose exec --no-TTY redis sh -ec '
        export REDISCLI_AUTH="$REDIS_PASSWORD"
        exec redis-cli --no-auth-warning --pipe
    '
    compose exec --no-TTY redis sh -ec '
        export REDISCLI_AUTH="$REDIS_PASSWORD"
        exec redis-cli --no-auth-warning --eval /opt/ramag-db-test/seed/redis.lua
    '
}

seed_mongo() {
    log "Resetting and seeding MongoDB"
    compose exec --no-TTY mongo sh -ec '
        exec mongosh --quiet \
            --username="$MONGO_INITDB_ROOT_USERNAME" \
            --password="$MONGO_INITDB_ROOT_PASSWORD" \
            --authenticationDatabase=admin \
            --file=/opt/ramag-db-test/seed/mongo.js
    '
}

verify_seed() {
    log "Verifying generated dataset sizes"

    local mysql_counts
    mysql_counts="$(compose exec --no-TTY mysql sh -ec '
        export MYSQL_PWD="$MYSQL_PASSWORD"
        mysql --batch --skip-column-names --host=127.0.0.1 --user="$MYSQL_USER" "$MYSQL_DATABASE" \
            --execute="SELECT (SELECT COUNT(*) FROM bulk_records), (SELECT COUNT(*) FROM type_matrix), (SELECT COUNT(*) FROM large_values), (SELECT COUNT(*) FROM spatial_samples)"
    ' | tr -d '\r' | tr '\t' ':' | tail -n 1)"
    [[ "$mysql_counts" == "100000:3:1:1" ]] || fail "Unexpected MySQL counts: $mysql_counts"

    local postgres_counts
    postgres_counts="$(compose exec --no-TTY postgres sh -ec '
        export PGPASSWORD="$POSTGRES_PASSWORD"
        psql --tuples-only --no-align --field-separator=: \
            --host=127.0.0.1 --username="$POSTGRES_USER" --dbname="$POSTGRES_DB" \
            --command="SELECT (SELECT COUNT(*) FROM public.bulk_records), (SELECT COUNT(*) FROM public.type_matrix), (SELECT COUNT(*) FROM public.large_values), (SELECT COUNT(*) FROM analytics.daily_metrics)"
    ' | tr -d '\r' | tail -n 1)"
    [[ "$postgres_counts" == "100000:3:1:8000" ]] || fail "Unexpected PostgreSQL counts: $postgres_counts"

    local redis_db0 redis_db15
    redis_db0="$(compose exec --no-TTY redis sh -ec '
        export REDISCLI_AUTH="$REDIS_PASSWORD"
        redis-cli --no-auth-warning --raw -n 0 DBSIZE
    ' | tr -d '\r' | tail -n 1)"
    redis_db15="$(compose exec --no-TTY redis sh -ec '
        export REDISCLI_AUTH="$REDIS_PASSWORD"
        redis-cli --no-auth-warning --raw -n 15 DBSIZE
    ' | tr -d '\r' | tail -n 1)"
    [[ "$redis_db0" =~ ^[0-9]+$ ]] && ((redis_db0 >= 46000)) || fail "Unexpected Redis DB 0 size: $redis_db0"
    [[ "$redis_db15" == "0" ]] || fail "Redis DB 15 must be empty before integration tests: $redis_db15"

    local mongo_counts
    mongo_counts="$(compose exec --no-TTY mongo sh -ec '
        mongosh --quiet \
            --username="$MONGO_INITDB_ROOT_USERNAME" \
            --password="$MONGO_INITDB_ROOT_PASSWORD" \
            --authenticationDatabase=admin \
            --eval="const d=db.getSiblingDB(process.env.RAMAG_DB_TEST_MONGO_DATABASE); print([d.users.countDocuments({}),d.products.countDocuments({}),d.orders.countDocuments({}),d.type_matrix.countDocuments({}),d.large_documents.countDocuments({}),d.metrics.countDocuments({}),d.capped_events.countDocuments({})].join(String.fromCharCode(58)))"
    ' | tr -d '\r' | tail -n 1)"
    [[ "$mongo_counts" == "20000:15000:60000:2:100:20000:10000" ]] || fail "Unexpected MongoDB counts: $mongo_counts"

    log "Dataset verification passed"
}

seed_databases() {
    start_databases
    log "Seed operation replaces data only inside the dedicated db-test volumes"
    seed_mysql
    seed_postgres
    seed_redis
    seed_mongo
    verify_seed
}

export_test_environment() {
    export RAMAG_TEST_MYSQL_HOST=127.0.0.1
    export RAMAG_TEST_MYSQL_PORT="$RAMAG_DB_TEST_MYSQL_PORT"
    export RAMAG_TEST_MYSQL_USER="$RAMAG_DB_TEST_MYSQL_USER"
    export RAMAG_TEST_MYSQL_PASSWORD="$RAMAG_DB_TEST_MYSQL_PASSWORD"
    export RAMAG_TEST_MYSQL_DB="$RAMAG_DB_TEST_MYSQL_DATABASE"

    export RAMAG_TEST_PG_HOST=127.0.0.1
    export RAMAG_TEST_PG_PORT="$RAMAG_DB_TEST_POSTGRES_PORT"
    export RAMAG_TEST_PG_USER="$RAMAG_DB_TEST_POSTGRES_USER"
    export RAMAG_TEST_PG_PASSWORD="$RAMAG_DB_TEST_POSTGRES_PASSWORD"
    export RAMAG_TEST_PG_DB="$RAMAG_DB_TEST_POSTGRES_DATABASE"

    export RAMAG_TEST_REDIS_HOST=127.0.0.1
    export RAMAG_TEST_REDIS_PORT="$RAMAG_DB_TEST_REDIS_PORT"
    export RAMAG_TEST_REDIS_USERNAME=''
    export RAMAG_TEST_REDIS_PASSWORD="$RAMAG_DB_TEST_REDIS_PASSWORD"

    export RAMAG_TEST_MONGO_HOST=127.0.0.1
    export RAMAG_TEST_MONGO_PORT="$RAMAG_DB_TEST_MONGO_PORT"
    export RAMAG_TEST_MONGO_DB="$RAMAG_DB_TEST_MONGO_DATABASE"
    export RAMAG_TEST_MONGO_USER="$RAMAG_DB_TEST_MONGO_USER"
    export RAMAG_TEST_MONGO_PASSWORD="$RAMAG_DB_TEST_MONGO_PASSWORD"

    export RAMAG_TEST_DATASET=full
    export RUST_TEST_THREADS=1
}

run_database_tests() {
    export_test_environment
    log "Running tests for the four database crates with integrations enabled"
    (cd "$REPO_DIR" && make _db-test-test)
}

run_quality_gate() {
    run_database_tests
    log "Running database crate compile, lint, and formatting gates"
    (cd "$REPO_DIR" && make _db-test-check)
    (cd "$REPO_DIR" && make _db-test-clippy)
    (cd "$REPO_DIR" && make _db-test-fmt)
    log "Full database test workflow passed"
}

run_existing_dataset_tests() {
    start_databases
    verify_seed
    run_database_tests
}

run_workspace_tests() {
    start_databases
    verify_seed
    export_test_environment
    log "Running the complete workspace test suite with all database integrations enabled"
    (cd "$REPO_DIR" && make test)
}

show_status() {
    prepare
    compose ps
    printf '\nLocal endpoints (credentials stay in .ramag/db-test.env):\n'
    printf '  MySQL:     127.0.0.1:%s/%s\n' "$RAMAG_DB_TEST_MYSQL_PORT" "$RAMAG_DB_TEST_MYSQL_DATABASE"
    printf '  PostgreSQL: 127.0.0.1:%s/%s\n' "$RAMAG_DB_TEST_POSTGRES_PORT" "$RAMAG_DB_TEST_POSTGRES_DATABASE"
    printf '  Redis:     127.0.0.1:%s (DB 0 data, DB 15 integration tests)\n' "$RAMAG_DB_TEST_REDIS_PORT"
    printf '  MongoDB:   127.0.0.1:%s/%s\n' "$RAMAG_DB_TEST_MONGO_PORT" "$RAMAG_DB_TEST_MONGO_DATABASE"
}

stop_databases() {
    prepare
    log "Stopping containers and preserving database volumes"
    compose down --remove-orphans
}

clean_databases() {
    prepare true
    log "Removing db-test containers, network, volumes, and local credentials"
    compose down --volumes --remove-orphans
    rm -f "$ENV_FILE"
    rmdir "$(dirname "$ENV_FILE")" 2>/dev/null || true
}

usage() {
    cat <<'USAGE'
Usage: db-test.sh <command>

Commands:
  all      Start, reset seed data, run all tests, then run quality gates
  up       Start the four databases and wait for health checks
  seed     Reset and regenerate all dedicated test datasets
  test     Run the four database crate tests against existing datasets
  workspace Run the complete workspace tests against existing datasets
  status   Show container health and local endpoints
  down     Stop containers but preserve volumes and credentials
  clean    Delete the dedicated containers, volumes, network, and credentials
USAGE
}

case "${1:-}" in
    all)
        seed_databases
        run_quality_gate
        ;;
    up)
        start_databases
        ;;
    seed)
        seed_databases
        ;;
    test)
        run_existing_dataset_tests
        ;;
    workspace)
        run_workspace_tests
        ;;
    status)
        show_status
        ;;
    down)
        stop_databases
        ;;
    clean)
        clean_databases
        ;;
    -h | --help | help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
