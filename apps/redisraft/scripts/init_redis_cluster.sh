#!/bin/bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source $SCRIPT_DIR/shared.sh

init_redis_cluster() {
	SIZE=$1
	echo "Initializing 5001"
	redis-cli -p 5001 raft.cluster init 
	sleep 1
	for i in $(seq 2 $SIZE); do
		echo "Joining 500$i to 5001"
		redis-cli -p 500$i RAFT.CLUSTER JOIN localhost:5001
	done
}

echo "Initializing Redis cluster with $CLUSTER_SIZE nodes"
init_redis_cluster $CLUSTER_SIZE
