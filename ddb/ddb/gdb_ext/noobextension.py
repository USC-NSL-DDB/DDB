import re
import gdb
import sys
from typing import List, Dict, Union


def read_runtime_const(varname, default):
    try:
        return int(gdb.parse_and_eval(varname))
    except Exception:
        return int(default)


G_IDLE = read_runtime_const("'runtime._Gidle'", 0)
G_RUNNABLE = read_runtime_const("'runtime._Grunnable'", 1)
G_RUNNING = read_runtime_const("'runtime._Grunning'", 2)
G_SYSCALL = read_runtime_const("'runtime._Gsyscall'", 3)
G_WAITING = read_runtime_const("'runtime._Gwaiting'", 4)
G_MORIBUND_UNUSED = read_runtime_const("'runtime._Gmoribund_unused'", 5)
G_DEAD = read_runtime_const("'runtime._Gdead'", 6)
G_ENQUEUE_UNUSED = read_runtime_const("'runtime._Genqueue_unused'", 7)
G_COPYSTACK = read_runtime_const("'runtime._Gcopystack'", 8)
G_SCAN = read_runtime_const("'runtime._Gscan'", 0x1000)
G_SCANRUNNABLE = G_SCAN+G_RUNNABLE
G_SCANRUNNING = G_SCAN+G_RUNNING
G_SCANSYSCALL = G_SCAN+G_SYSCALL
G_SCANWAITING = G_SCAN+G_WAITING

sts = {
    G_IDLE: 'idle',
    G_RUNNABLE: 'runnable',
    G_RUNNING: 'running',
    G_SYSCALL: 'syscall',
    G_WAITING: 'waiting',
    G_MORIBUND_UNUSED: 'moribund',
    G_DEAD: 'dead',
    G_ENQUEUE_UNUSED: 'enqueue',
    G_COPYSTACK: 'copystack',
    G_SCAN: 'scan',
    G_SCANRUNNABLE: 'runnable+s',
    G_SCANRUNNING: 'running+s',
    G_SCANSYSCALL: 'syscall+s',
    G_SCANWAITING: 'waiting+s',
}
print("Loading Go Runtime support.", file=sys.stderr)
currentGoroutine = -1
save_frame = None


def pc_to_int(pc):
    # python2 will not cast pc (type void*) to an int cleanly
    # instead python2 and python3 work with the hex string representation
    # of the void pointer which we can parse back into an int.
    # int(pc) will not work.
    try:
        # python3 / newer versions of gdb
        pc = int(pc)
    except gdb.error:
        # str(pc) can return things like
        # "0x429d6c <runtime.gopark+284>", so
        # chop at first space.
        pc = int(str(pc).split(None, 1)[0], 16)
    return pc


class SliceValue:
    "Wrapper for slice values."

    def __init__(self, val):
        self.val = val

    @property
    def len(self):
        return int(self.val['len'])

    @property
    def cap(self):
        return int(self.val['cap'])

    def __getitem__(self, i):
        if i < 0 or i >= self.len:
            raise IndexError(i)
        ptr = self.val["array"]
        return (ptr + i).dereference()


class GoroutinesCmd(gdb.Command):
    "List all goroutines."

    def __init__(self):
        gdb.Command.__init__(self, "info goroutines",
                             gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, _arg, _from_tty):
        # args = gdb.string_to_argv(arg)
        vp = gdb.lookup_type('void').pointer()
        for ptr in SliceValue(gdb.parse_and_eval("'runtime.allgs'")):
            if ptr['atomicstatus']['value'] == G_DEAD:
                continue
            s = ' '
            if ptr['m']:
                s = '*'
            pc = ptr['sched']['pc'].cast(vp)
            pc = pc_to_int(pc)
            blk = gdb.block_for_pc(pc)
            status = int(ptr['atomicstatus']['value'])
            st = sts.get(status, "unknown(%d)" % status)
            print(s, ptr['goid'], "{0:8s}".format(st), blk.function)


class GoroutineSwitchCmd(gdb.Command):
    "List all goroutines."

    def __init__(self):
        gdb.Command.__init__(
            self, "goroutine", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, _arg, _from_tty):
        global globalvar1
        print(globalvar1)
        globalvar1 += 1


class GoroutineCmd(gdb.Command):
    """Execute gdb command in the context of goroutine <goid>.

    Switch PC and SP to the ones in the goroutine's G structure,
    execute an arbitrary gdb command, and restore PC and SP.

    Usage: (gdb) goroutine <goid> <gdbcmd>

    You could pass "all" as <goid> to apply <gdbcmd> to all goroutines.

    For example: (gdb) goroutine all <gdbcmd>

    Note that it is ill-defined to modify state in the context of a goroutine.
    Restrict yourself to inspecting values.
    """

    def __init__(self):
        gdb.Command.__init__(
            self, "goroutine", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, arg, _from_tty):
        goid_str, = arg.split(None, 1)
        goids = []

        if goid_str == 'all':
            for ptr in SliceValue(gdb.parse_and_eval("'runtime.allgs'")):
                goids.append(int(ptr['goid']))
        else:
            goids = [int(gdb.parse_and_eval(goid_str))]

        for goid in goids:
            self.invoke_per_goid(goid)

    def invoke_per_goid(self, goid):
        global save_frame
        pc, sp = find_goroutine(goid)
        if not pc:
            print("No such goroutine: ", goid)
            return
        pc = pc_to_int(pc)
        save_frame = gdb.selected_frame()
        gdb.parse_and_eval('$save_sp = $sp')
        gdb.parse_and_eval('$save_pc = $pc')
        # In GDB, assignments to sp must be done from the
        # top-most frame, so select frame 0 first.
        gdb.execute('select-frame 0')
        gdb.parse_and_eval('$sp = {0}'.format(str(sp)))
        gdb.parse_and_eval('$pc = {0}'.format(str(pc)))
        # try:
        # 	gdb.execute(cmd)
        # finally:
        # 	# In GDB, assignments to sp must be done from the
        # 	# top-most frame, so select frame 0 first.
        # 	gdb.execute('select-frame 0')
        # 	gdb.parse_and_eval('$pc = $save_pc')
        # 	gdb.parse_and_eval('$sp = $save_sp')
        # 	save_frame.select()


class ContextSwitchingCmd(gdb.MICommand):

    def __init__(self):
        super().__init__("-switch-context-custom")

    def invoke(self, args):
        try:
            cur_rip = int(args[0])
            cur_rsp = int(args[1])
            gdb.parse_and_eval('$save_sp = $sp')
            gdb.parse_and_eval('$save_pc = $pc')
            gdb.execute('select-frame 0')
            gdb.parse_and_eval('$sp = {0}'.format(str(cur_rsp)))
            gdb.parse_and_eval('$pc = {0}'.format(str(cur_rip)))
            original_sp = int(gdb.parse_and_eval('$save_sp'))
            original_pc = int(gdb.parse_and_eval('$save_pc'))
            return {"message": "success", "rip": original_pc, "rsp": original_sp}
        except Exception as e:
            return {"message": "error", "rip": None, "rsp": None}


ContextSwitchingCmd()


class RestoreContext(gdb.MICommand):
    def __init__(self):
        super().__init__("-restore-context-custom")

    def invoke(self, args):
        try:
            global save_frame
            gdb.execute('select-frame 0')
            gdb.parse_and_eval('$pc = $save_pc')
            gdb.parse_and_eval('$sp = $save_sp')
            save_frame.select()
            return {"message": "success"}
        except Exception as e:
            return {"message": "error"}


class GetRemoteBTInfo(gdb.MICommand):
    def __init__(self):
        super().__init__("-get-remote-bt")

    def invoke(self, args):
        try:
            frame = gdb.selected_frame()
            frames: List[gdb.Frame] = []
            while frame is not None and frame.is_valid():
                frames.append(frame)
                frame = frame.older()
            ip_address = []
            port = -1
            parent_rsp = -1
            parent_rip = -1
            message = "success"
            print("before iterating frames")
            for cur_frame in frames:
                if cur_frame.function() is not None and cur_frame.function().name.endswith("runHandler"):
                    print("found")
                    for symbol in cur_frame.block():
                        if symbol.is_argument or symbol.is_variable:
                            if symbol.name == "msg":
                                slice_val = symbol.value(cur_frame)
                                data_ptr = slice_val['array']
                                length = int(slice_val['len'])
                                byte_array_type = gdb.lookup_type(
                                    "uint8").array(length - 1).pointer()
                                data_ptr_casted = data_ptr.cast(
                                    byte_array_type)
                                # Now, you can read the bytes
                                byte_array = bytearray()
                                for i in range(length):
                                    byte_array.append(
                                        int(data_ptr_casted.dereference()[i]))

                                # Convert to a Python bytes object if necessary
                                bytes_object = bytes(byte_array)
                                metadata = bytes_object[49:65]
                                # Convert these byte segments to integers using little-endian encoding
                                parent_rsp = int.from_bytes(
                                    metadata[0:8], byteorder='little')
                                parent_rip = int.from_bytes(
                                    metadata[-8:], byteorder='little')

                                print(f"parent_rsp: {parent_rsp}")
                                print(f"parent_rip {parent_rip}")
                            if symbol.name == "c":
                                sc = symbol.value(cur_frame).dereference()
                                tcp_conn_type = gdb.lookup_type(
                                    'net.TCPConn').pointer()
                                tcp_addr_type = gdb.lookup_type(
                                    'net.TCPAddr').pointer()
                                fd = sc["c"]["data"].cast(tcp_conn_type).dereference()[
                                    "conn"]["fd"].dereference()
                                parent_addr = fd["raddr"]["data"].cast(
                                    tcp_addr_type).dereference()
                                port = int(parent_addr['Port'])
                                ip_address = [int(b)
                                              for b in SliceValue(parent_addr['IP'])]
        except Exception as e:
            print(e)
            message = "error"
        finally:
            return {"message": message, "metadata": {"parentRIP": parent_rip, "parentRSP": parent_rsp, "parentAddr": ip_address, "parentPort": port}}


class GetRemoteBTInfoInContext(gdb.MICommand):
    def __init__(self):
        super().__init__("-get-remote-bt-in-context")

    def invoke(self, args):
        cur_rip, cur_rsp = args[0], args[1]
        # switch to context
        saved_frame = gdb.selected_frame()
        gdb.parse_and_eval('$save_sp = $sp')
        gdb.parse_and_eval('$save_pc = $pc')
        # In GDB, assignments to sp must be done from the
        # top-most frame, so select frame 0 first.
        gdb.execute('select-frame 0')
        gdb.parse_and_eval('$sp = {0}'.format(str(cur_rsp)))
        gdb.parse_and_eval('$pc = {0}'.format(str(cur_rip)))
        ip_address = []
        port = -1
        parent_rip = -1
        parent_rsp = -1
        backtrace_info = []
        try:
            frame = gdb.selected_frame()
            frames: List[gdb.Frame] = []
            while frame is not None and frame.is_valid() and frame.function() is not None:
                frames.append(frame)
                backtrace_info.append({
                    "level": len(backtrace_info),
                    "addr": frame.pc(),
                    "func": frame.function().name, "line": frame.find_sal().line,
                    "file": frame.find_sal().symtab.filename if frame.find_sal().symtab else "unknown",
                    "fullname":	frame.find_sal().symtab.fullname if frame.find_sal().symtab else "unknown",
                    "line": frame.find_sal().line,
                    "arch": frame.architecture().name,
                })
                frame = frame.older()
            for cur_frame in frames:
                if cur_frame.function() is not None and cur_frame.function().name.endswith("runHandler"):
                    print("found")
                    for symbol in cur_frame.block():
                        if symbol.is_argument or symbol.is_variable:
                            if symbol.name == "msg":
                                slice_val = symbol.value(cur_frame)
                                data_ptr = slice_val['array']
                                length = int(slice_val['len'])
                                byte_array_type = gdb.lookup_type(
                                    "uint8").array(length - 1).pointer()
                                data_ptr_casted = data_ptr.cast(
                                    byte_array_type)
                                # Now, you can read the bytes
                                byte_array = bytearray()
                                for i in range(length):
                                    byte_array.append(
                                        int(data_ptr_casted.dereference()[i]))

                                # Convert to a Python bytes object if necessary
                                bytes_object = bytes(byte_array)
                                metadata = bytes_object[49:65]
                                # Convert these byte segments to integers using little-endian encoding
                                parent_rsp = int.from_bytes(
                                    metadata[0:8], byteorder='little')
                                parent_rip = int.from_bytes(
                                    metadata[-8:], byteorder='little')

                                print(f"parent_rsp: {parent_rsp}")
                                print(f"parent_rip {parent_rip}")
                            if symbol.name == "c":
                                sc = symbol.value(cur_frame).dereference()
                                tcp_conn_type = gdb.lookup_type(
                                    'net.TCPConn').pointer()
                                tcp_addr_type = gdb.lookup_type(
                                    'net.TCPAddr').pointer()
                                fd = sc["c"]["data"].cast(tcp_conn_type).dereference()[
                                    "conn"]["fd"].dereference()
                                parent_addr = fd["raddr"]["data"].cast(
                                    tcp_addr_type).dereference()
                                port = int(parent_addr['Port'])
                                ip_address = [
                                    int(b) for b in SliceValue(parent_addr['IP'])]
        finally:
            gdb.execute('select-frame 0')
            gdb.parse_and_eval('$pc = $save_pc')
            gdb.parse_and_eval('$sp = $save_sp')
            saved_frame.select()
        return {"stack": backtrace_info, "metadata": {"parentRIP": parent_rip, "parentRSP": parent_rsp, "parentAddr": ip_address, "parentPort": port}}


def extract_array_info(msg_value):
    match = re.search(r'array = (0x[0-9a-fA-F]+)', msg_value)
    if match:
        array_address = int(match.group(1), 16)
        return array_address
    return None


class ListStackVars(gdb.MICommand):
    """List local variables in the current stack frame with their values"""

    def __init__(self):
        super().__init__("-list-stack-vars")

    def invoke(self, args):
        frame = gdb.selected_frame()
        block = frame.block()

        # Check if we have any local variables
        if block is None:
            return {'error': 'No locals in a synthetic block'}

        variables = []
        for symbol in block:
            # Filter out symbols that are not local variables
            if symbol.is_argument or symbol.is_variable:
                try:
                    name = symbol.name
                    value = frame.read_var(symbol)
                    variables.append({'name': name, 'value': str(value)})
                except gdb.error:  # Error evaluating a variable's value
                    variables.append({'name': name, 'value': '<error>'})

        return {'variables': variables}


def find_goroutine(goid):
    """
    find_goroutine attempts to find the goroutine identified by goid.
    It returns a tuple of gdb.Value's representing the stack pointer
    and program counter pointer for the goroutine.

    @param int goid

    @return tuple (gdb.Value, gdb.Value)
    """
    vp = gdb.lookup_type('void').pointer()
    for ptr in SliceValue(gdb.parse_and_eval("'runtime.allgs'")):
        if ptr['atomicstatus']['value'] == G_DEAD:
            continue
        if ptr['goid'] == goid:
            break
    else:
        return None, None
    # Get the goroutine's saved state.
    pc, sp = ptr['sched']['pc'], ptr['sched']['sp']
    status = ptr['atomicstatus']['value'] & ~G_SCAN
    # Goroutine is not running nor in syscall, so use the info in goroutine
    if status != G_RUNNING and status != G_SYSCALL:
        return pc.cast(vp), sp.cast(vp)

    # If the goroutine is in a syscall, use syscallpc/sp.
    pc, sp = ptr['syscallpc'], ptr['syscallsp']
    if sp != 0:
        return pc.cast(vp), sp.cast(vp)
    # Otherwise, the goroutine is running, so it doesn't have
    # saved scheduler state. Find G's OS thread.
    m = ptr['m']
    if m == 0:
        return None, None
    for thr in gdb.selected_inferior().threads():
        if thr.ptid[1] == m['procid']:
            break
    else:
        return None, None
    # Get scheduler state from the G's OS thread state.
    curthr = gdb.selected_thread()
    try:
        thr.switch()
        pc = gdb.parse_and_eval('$pc')
        sp = gdb.parse_and_eval('$sp')
    finally:
        curthr.switch()
    return pc.cast(vp), sp.cast(vp)


ListStackVars()  # Instantiate the command to register it with GDB
GoroutinesCmd()
GoroutineCmd()
RestoreContext()
GetRemoteBTInfo()
GetRemoteBTInfoInContext()
