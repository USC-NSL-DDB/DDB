# !!! NOTE: source this file instead of executing it.

# Check if the script is being sourced
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
	echo "Error: This script should be sourced, not executed."
	echo "Usage: source $0 y|n|check"
	exit 1
fi

# Check if at least one argument is provided
if [ $# -lt 1 ]; then
	echo "Usage: source $0 y|n|check"
fi

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Define the path to libfaketime
LIBFAKETIME_PATH="$LIBFAKETIME_DIR/src/libfaketime.so.1"

if [ "$1" = "y" ]; then
	# Check if libfaketime exists
	if [ -f "$LIBFAKETIME_PATH" ]; then
		LIBFAKETIME_DIR="$SCRIPT_DIR/../libfaketime"
		LIBFAKETIME_FLAGS="FAKETIME_NO_CACHE=1 FAKETIME=\"-00000000000000000\""

		pushd $LIBFAKETIME_DIR
		make
		popd

		export LD_PRELOAD="$LIBFAKETIME_PATH"
		echo "Faketime enabled in this shell session"
	else
		echo "Error: libfaketime not found at $LIBFAKETIME_PATH"
	fi
elif [ "$1" = "n" ]; then
	# Unset LD_PRELOAD to disable faketime
	unset LD_PRELOAD
	echo "Faketime disabled in this shell session"
elif [ "$1" = "check" ]; then
	# Check if LD_PRELOAD is set to libfaketime
	PRELOAD=$(echo $LD_PRELOAD | grep -o "$LIBFAKETIME_PATH")
	if [ "$PRELOAD" = "$LIBFAKETIME_PATH" ]; then
		echo "Faketime is enabled in this shell session"
	else
		echo "Faketime is disabled in this shell session"
	fi
else
	echo "Invalid argument: $1. Use 'y' or 'n'."
fi
