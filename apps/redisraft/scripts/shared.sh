SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

CLUSTER_SIZE=3
USE_FAKETIME=true
# USE_FAKETIME=false

RAFTLOGS_DIR=$SCRIPT_DIR/../raftlogs
REDISLOGS_DIR=$SCRIPT_DIR/../redislogs
APP_RUNNER=${HOME}/.local/bin/ddb_runapp

prep_raft_target() {
	mkdir -p $RAFTLOGS_DIR
	mkdir -p $REDISLOGS_DIR
	
	VALID_BUG_TYPES=("75b010" "none" "a1b2c3" "x9y8z7" "d4e5f6")
	if [[ -z "$BUG_TYPE" ]]; then
		echo "Please set BUG_TYPE to one of: ${VALID_BUG_TYPES[*]}"
		read -p "Enter BUG_TYPE: " BUG_TYPE
	fi

	# Validate BUG_TYPE or prompt user to set it
	if [[ ! " ${VALID_BUG_TYPES[@]} " =~ " ${BUG_TYPE} " ]]; then
		echo "Invalid BUG_TYPE: $BUG_TYPE"
		echo "Please set BUG_TYPE to one of: ${VALID_BUG_TYPES[*]}"
		echo "Example: BUG_TYPE=75b010 ./your_script.sh"
		return 1
	fi
	
	export REDISRAFT_DIR="$SCRIPT_DIR/../redisraft-bug-raft-$BUG_TYPE"
}
