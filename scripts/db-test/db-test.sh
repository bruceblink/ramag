#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/compose.yaml"
SEED_DIR="$SCRIPT_DIR/seed"
ENV_FILE="${RAMAG_DB_TEST_ENV_FILE:-$REPO_DIR/.ramag/db-test.env}"
PROJECT_NAME="ramag-db-test"

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

random_secret() {
    od -An -N24 -tx1 /dev/urandom | tr -d ' \n'
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

# 凭据仅保存在 Git 已忽略的 .ramag 目录，数据库卷与凭据必须成对保留。
create_environment() {
    local allow_existing_volumes="${1:-false}"
    if [[ -f "$ENV_FILE" ]]; then
        chmod 600 "$ENV_FILE"
        return
    fi
    if [[ "$allow_existing_volumes" != "true" ]] && managed_volumes_exist; then
        fail "Credentials are missing while managed volumes still exist. Run 'make db-test-clean' to remove the isolated test data, then retry."
    fi

    mkdir -p "$(dirname "$ENV_FILE")"
    umask 077
    {
        printf 'RAMAG_DB_TEST_MYSQL_PORT=%s\n' "${RAMAG_DB_TEST_MYSQL_PORT:-13306}"
        printf 'RAMAG_DB_TEST_MYSQL_DATABASE=ramag_test\n'
        printf 'RAMAG_DB_TEST_MYSQL_USER=ramag\n'
        printf 'RAMAG_DB_TEST_MYSQL_PASSWORD=%s\n' "$(random_secret)"
        printf 'RAMAG_DB_TEST_MYSQL_ROOT_PASSWORD=%s\n' "$(random_secret)"
        printf 'RAMAG_DB_TEST_POSTGRES_PORT=%s\n' "${RAMAG_DB_TEST_POSTGRES_PORT:-15432}"
        printf 'RAMAG_DB_TEST_POSTGRES_DATABASE=ramag_test\n'
        printf 'RAMAG_DB_TEST_POSTGRES_USER=ramag\n'
        printf 'RAMAG_DB_TEST_POSTGRES_PASSWORD=%s\n' "$(random_secret)"
        printf 'RAMAG_DB_TEST_REDIS_PORT=%s\n' "${RAMAG_DB_TEST_REDIS_PORT:-16379}"
        printf 'RAMAG_DB_TEST_REDIS_PASSWORD=%s\n' "$(random_secret)"
        printf 'RAMAG_DB_TEST_MONGO_PORT=%s\n' "${RAMAG_DB_TEST_MONGO_PORT:-27018}"
        printf 'RAMAG_DB_TEST_MONGO_DATABASE=ramag_demo\n'
        printf 'RAMAG_DB_TEST_MONGO_USER=ramag\n'
        printf 'RAMAG_DB_TEST_MONGO_PASSWORD=%s\n' "$(random_secret)"
    } >"$ENV_FILE"
    chmod 600 "$ENV_FILE"
    log "Created local credentials at .ramag/db-test.env"
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
