import sys
import gdb

print("Loading distributed backtrace support.", file=sys.stderr)

# allow to manually reload while developing
goobjfile = gdb.current_objfile() or gdb.objfiles()[0]
goobjfile.pretty_printers = []

class DistributedBTCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "dbt", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, _arg, _from_tty):
        print("executed dbt")

DistributedBTCmd()