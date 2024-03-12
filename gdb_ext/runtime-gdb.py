import sys
import subprocess
import gdb

print("Loading distributed backtrace support.", file=sys.stderr)

# allow to manually reload while developing
# goobjfile = gdb.current_objfile() or gdb.objfiles()[0]
# goobjfile.pretty_printers = []

def handle_invoke():
    result = gdb.execute_mi("-stack-list-frames")
    print(f"result:\n{result}")
    

class DistributedBTCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "dbt", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        # gdb.Command.__init__(self, "dbacktrace", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        self.mi_cmd = dbt_mi_cmd

    def invoke(self, _arg, _from_tty):
        # handle_invoke()
        # result = gdb.execute_mi("-stack-list-distributed-frames")
        # print(f"result:\n{result}")
        self.mi_cmd.invoke(None)
        # command = 'nc localhost 12345'
        # result = subprocess.run(command, input=input_data, shell=True, text=True, capture_output=True)

        # # Capture the output
        # output = result.stdout

        # # Print the output
        # print(output)
        print("executed dbt")

class DistributedBacktraceMICmd(gdb.MICommand):
    def __init__(self):
        super(DistributedBacktraceMICmd, self).__init__("-stack-list-distributed-frames")

    def invoke(self, argv):
        print(type(argv))
        result = gdb.execute_mi("-stack-list-frames")
        print(f"result:\n{result}")
        # handle_invoke()

class MIEcho(gdb.MICommand):
    """Echo arguments passed to the command."""

    def __init__(self, name, mode):
        self._mode = mode
        super(MIEcho, self).__init__(name)

    def invoke(self, argv):
        if self._mode == 'dict':
            return { 'dict': { 'argv' : argv } }
        elif self._mode == 'list':
            return { 'list': argv }
        else:
            return { 'string': ", ".join(argv) }


MIEcho("-echo-dict", "dict")
MIEcho("-echo-list", "list")
MIEcho("-echo-string", "string")

dbt_mi_cmd = DistributedBacktraceMICmd()
dbt_cmd = DistributedBTCmd()