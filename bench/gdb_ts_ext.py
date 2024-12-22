import gdb
import time

class TimestampCommand(gdb.Command):
    """Run a GDB command and print its result with a timestamp."""

    def __init__(self):
        super(TimestampCommand, self).__init__("timestamp", gdb.COMMAND_USER)

    def invoke(self, arg, from_tty):
        start_time = time.time()
        result = gdb.execute(arg, to_string=True)  # Execute GDB command
        end_time = time.time()

        # Print the command, result, and timestamps
        print(f"Command: {arg}")
        print(f"Timestamp (start): {start_time}")
        print(f"Result:\n{result}")
        print(f"Timestamp (end): {end_time}")
        print(f"Duration: {end_time - start_time:.6f} seconds")

TimestampCommand()
