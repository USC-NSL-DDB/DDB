#!/bin/bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source $SCRIPT_DIR/shared.sh

RAFTLOGS_DIR=$SCRIPT_DIR/../raftlogs

start_one_redis_bg() {
	IDX=$1
	echo "Starting Redis server on port 500$IDX"
	redis-server \
		--port 500$IDX --dbfilename raft$IDX.rdb \
		--loadmodule ../redisraft/redisraft.so \
		--raft.log-filename raftlog$IDX.db \
		--raft.id $IDX \
		--raft.addr localhost:500$IDX --raft.log-max-file-size 1280000 --raft.log-max-cache-size 640000 \
		--raft.enable-ddb yes > /dev/null 2>&1 &
}

start_one_redis_bg_with_faketime() {
	IDX=$1
	echo "Starting Redis server on port 500$IDX"
	${APP_RUNNER} redis-server \
		--port 500$IDX --dbfilename raft$IDX.rdb \
		--loadmodule ../redisraft/redisraft.so \
		--raft.log-filename raftlog$IDX.db \
		--raft.id $IDX \
		--raft.addr localhost:500$IDX --raft.log-max-file-size 1280000 --raft.log-max-cache-size 640000 \
		--raft.enable-ddb yes > /dev/null 2>&1 &
}

start_redis_cluster() {
	SIZE=$1
	mkdir -p $RAFTLOGS_DIR
	pushd $RAFTLOGS_DIR
	for i in $(seq 1 $SIZE); do
		if [ "$USE_FAKETIME" = true ]; then
			start_one_redis_bg_with_faketime $i
		else
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

echo "Starting Redis cluster with $CLUSTER_SIZE nodes"
start_redis_cluster $CLUSTER_SIZE
