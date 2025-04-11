#!/bin/bash

# Start the Raft server

function start_node {
	./build/app/raft_node --enable_ddb --ddb_addr 127.0.0.1 --ni >/dev/null 2>&1 &
}

function stop {
	killall raft_node
}

function status {
	ps aux | grep raft_node | grep -v grep
}

function killall {
	pkill -f -9 raft_node
	echo "All raft_node processes terminated"
}

function start_multi {
	count=$1
	for i in $(seq 1 $count); do
		start_node
	done
	echo "Started $count raft nodes"
}

if [ "$1" = "start" ] && [ "$2" -gt 0 ]; then
	start_multi "$2"
elif [ "$1" = "stop" ]; then
	stop
elif [ "$1" = "status" ]; then
	status
elif [ "$1" = "kill" ]; then
	killall
else
	echo "Usage: $0 {start <count>|stop|status|kill}"
fi
