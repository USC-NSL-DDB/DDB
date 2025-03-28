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
		--raft.addr localhost:500$IDX --raft.log-max-file-size 1280000 --raft.log-max-cache-size 640000 \
		--raft.enable-ddb yes > /dev/null 2>&1 &
}

start_redis() {
	SIZE=$1
	mkdir -p $RAFTLOGS_DIR
	pushd $RAFTLOGS_DIR
	for i in $(seq 1 $SIZE); do
		start_one_redis_bg $i
	done
	popd
}

start_redis_cluster() {
	SIZE=$1
	start_redis $SIZE
}

echo "Starting Redis cluster with $CLUSTER_SIZE nodes"
start_redis_cluster $CLUSTER_SIZE
