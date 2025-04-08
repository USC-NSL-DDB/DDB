#!/bin/bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source $SCRIPT_DIR/shared.sh
prep_raft_target

RAFT_MODULE=$REDISRAFT_DIR/redisraft.so

start_one_redis_bg() {
	IDX=$1
	echo "Starting Redis server on port 500$IDX"
	redis-server \
		--port 500$IDX --dbfilename raft$IDX.rdb \
		--loadmodule $RAFT_MODULE \
		--raft.log-filename raftlog$IDX.db \
		--raft.id $IDX \
		--raft.addr localhost:500$IDX --raft.log-max-file-size 1280000 --raft.log-max-cache-size 640000 \
		--raft.enable-ddb yes > "$REDISLOGS_DIR/redis-server-$IDX.log" 2>&1 &
}

start_one_redis_bg_with_faketime() {
	IDX=$1
	echo "Starting Redis server on port 500$IDX"
	${APP_RUNNER} redis-server \
		--port 500$IDX --dbfilename raft$IDX.rdb \
		--loadmodule $RAFT_MODULE \
		--raft.log-filename raftlog$IDX.db \
		--raft.id $IDX \
		--raft.addr localhost:500$IDX --raft.log-max-file-size 1280000 --raft.log-max-cache-size 640000 \
		--raft.enable-ddb yes > "$REDISLOGS_DIR/redis-server-$IDX.log" 2>&1 &
}

build_redisraft_module() {
	CLEAN_FIRST=${1:-false}
	# Ensure the RedisRaft module is built
	echo "Building RedisRaft module..."
	pushd $REDISRAFT_DIR
	mkdir -p build
	cd build
	if [ "$CLEAN_FIRST" = true ]; then
		rm -rf *
	fi
	cmake -DCMAKE_BUILD_TYPE=Debug ..
	make -j$(nproc)
	if [ $? -ne 0 ]; then
		echo "Failed to build RedisRaft module"
		exit 1
	fi
	popd
}

start_redis_cluster() {
	build_redisraft_module
	SIZE=$1
	mkdir -p $RAFTLOGS_DIR
	mkdir -p $REDISLOGS_DIR
	pushd $RAFTLOGS_DIR
	for i in $(seq 1 $SIZE); do
		if [ "$USE_FAKETIME" = true ]; then
			echo "FAKETIME enabled"
			start_one_redis_bg_with_faketime $i
		else
			echo "FAKETIME disabled"
			start_one_redis_bg $i
		fi
	done
	popd
}

# Check if faketime is requested but APP_RUNNER is not set
if [ "$USE_FAKETIME" = true ] && [ -z "${APP_RUNNER}" ]; then
		echo "Error: --faketime option requires APP_RUNNER to be set"
		echo "Please specify ddb_runapp or install it at $APP_RUNNER first."
		echo "Example: export DDB_AppRunner=./path/to/ddb_runapp"
		exit 1
fi

echo "Using raft module: $RAFT_MODULE"
echo "Starting Redis cluster with $CLUSTER_SIZE nodes"
start_redis_cluster $CLUSTER_SIZE
