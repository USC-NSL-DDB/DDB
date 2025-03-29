SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

CLUSTER_SIZE=3
USE_FAKETIME=true
# USE_FAKETIME=false

RAFTLOGS_DIR=$SCRIPT_DIR/../raftlogs
APP_RUNNER=${HOME}/.local/bin/ddb_runapp
