import sys
from typing import List, Optional
import socket
import struct
import gdb

print("Loading distributed backtrace support.", file=sys.stderr)

# allow to manually reload while developing
# goobjfile = gdb.current_objfile() or gdb.objfiles()[0]
# goobjfile.pretty_printers = []


class DistributedBTCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "dbt", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        # gdb.Command.__init__(self, "dbacktrace", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        self.mi_cmd = dbt_mi_cmd

    def invoke(self, _arg, _from_tty):
        # handle_invoke()
        # result = gdb.execute_mi("-stack-list-distributed-frames")
        # print(f"result:\n{result}")
        # print(gdb.execute("bt"))
        result = self.mi_cmd.invoke(None)
        
        if "stack" in result:
            stacks = result["stack"]
            for stack in stacks:
                print(f"{stack['level']} {stack['func']} file:{stack['file']}") 
        else:
            print("no stack info presented")
        # command = 'nc localhost 12345'
        # result = subprocess.run(command, input=input_data, shell=True, text=True, capture_output=True)

        # # Capture the output
        # output = result.stdout

        # # Print the output
        # print(output)
        print("executed dbt")


def get_local_variables(frame: gdb.Frame) -> List[gdb.Symbol]:
    """Get all local variables (symbols) of the given frame."""
    if frame is None:
        print("No frame is currently selected.")
        return None

    local_vals: List[gdb.Symbol] = []
    # Iterate through the block for the selected frame
    # Blocks can contain symbols such as variables
    block = frame.block()
    while block:
        if block.is_global:
            break
        for symbol in block:
            # Check if the symbol is a variable
            if symbol.is_variable:
                local_vals.append(symbol)
        block = block.superblock
    return local_vals


def int_to_ip(ip_int: int) -> str:
    return socket.inet_ntoa(struct.pack('!I', ip_int))

# Function to fetch and print the global variable


def get_global_variable(var_name, to_print: bool = False, check_is_var: bool = True) -> gdb.Value:
    try:
        var = gdb.lookup_symbol(var_name)[0]
        # check_is_var is used for this specific case where the
        # globally defined variable is not recognized as a variable by gdb.
        is_var = True if (not check_is_var) else var.is_variable
        if var is not None and is_var:
            value = var.value()
            if to_print:
                print(f"Value of {var_name}: {value}")
            return value
        else:
            print(f"No such global variable: {var_name}")
            return None
    except gdb.error as e:
        print(f"Error accessing variable: {str(e)}")
        return None


class GetGlobalVarCommand(gdb.Command):
    """A custom command to fetch a global variable"""

    def __init__(self):
        super(GetGlobalVarCommand, self).__init__("get-global-var",
                                                  gdb.COMMAND_DATA,
                                                  gdb.COMPLETE_SYMBOL)

    def invoke(self, arg, from_tty):
        args = gdb.string_to_argv(arg)
        if len(args) != 1:
            print("Usage: get-global-var variable_name")
        else:
            get_global_variable(args[0], to_print=True)


class DistributedBacktraceMICmd(gdb.MICommand):
    def __init__(self):
        super(DistributedBacktraceMICmd, self).__init__(
            "-stack-list-distri-frames")

    def invoke(self, argv):
        from pprint import pprint
        result = gdb.execute_mi("-stack-list-frames")

        frame = gdb.selected_frame()
        frames: List[gdb.Frame] = []
        while frame and frame.pc():
            frames.append(frame)
            frame = frame.older()

        remote_ip: Optional[int] = None
        # remote_port: Optional[int] = None
        local_ip: Optional[int] = None
        # local_port: Optional[int] = None
        parent_rip: Optional[int] = None
        parent_rsp: Optional[int] = None
        parent_rbp: Optional[int] = None
        pid: Optional[int] = None

        is_remote_call = False

        for cur_frame in frames:
            curr_func = cur_frame.function()
            if curr_func and curr_func.name.startswith("nu::RPCServer::handler_fn"):
                # print("found")
                is_remote_call = True
                for sym in get_local_variables(cur_frame):
                    if sym.name == "meta":
                        # print("found meta")
                        val = sym.value(cur_frame)
                        # print("found val")
                        remote_ip = int(val['meta']['caller_comm_ip'])
                        # print("found ip")
                        pid = int(val['meta']['pid'])
                        # print("found pid")
                        parent_rip = int(val['ctx']['rip'])
                        parent_rsp = int(val['ctx']['rsp'])
                        parent_rbp = int(val['ctx']['rbp'])
                        # print("found ctx")
                        break
                        # print(f"caller ip: {int_to_ip(remote_ip)}")
                        # print(f"rip: {parent_rip:#x}")
                        # print(f"rsp: {parent_rsp:#x}")
                        # print(f"rbp: {parent_rbp:#x}")
                        # print(f"pid: {pid}")
            if is_remote_call:
                break
        print(f"ip: {remote_ip}, pid: {pid}, rip: {parent_rip}, rsp: {parent_rsp}, rbp: {parent_rbp}")
        # print("get all data")

        if not is_remote_call:
            # print("Did not find a valid remote call")
            return result
        
        ddb_meta = get_global_variable(
            "ddb_meta", to_print=False, check_is_var=False)
        if ddb_meta:
            local_ip = int(ddb_meta["comm_ip"])
            # print(f"local ip: {int_to_ip(local_ip)}")
        else:
            print("Failed to find ddb_meta")

        if remote_ip is None or local_ip is None:
            print("Failed to find remote/local address")
            return result

        if parent_rip is None or parent_rsp is None:
            print("Failed to find parent rip/rsp")
            return result

        backtrace_meta = {
            "remote_addr": {
                "ip": remote_ip,
            },
            "local_addr": {
                "ip": local_ip,
            },
            "caller_meta": {
                "rip": parent_rip,
                "rsp": parent_rsp,
                "rbp": parent_rbp,
                "pid": pid
            }
        }
        result["bt_meta"] = backtrace_meta
        # pprint(result)
        return result


saved_frame = None

class Registers:
    def __init__(self, rip, rsp, rbp):
        self.rip = rip
        self.rsp = rsp
        self.rbp = rbp

    def __str__(self):
        return f"rip: {self.rip:#x}, rsp: {self.rsp:#x}, rbp: {self.rbp:#x}"

def switch_context(regs: Registers) -> gdb.Frame:
    # switch to context
    saved_frame = gdb.selected_frame()
    gdb.parse_and_eval('$save_sp = $sp')
    gdb.parse_and_eval('$save_pc = $pc')
    gdb.parse_and_eval('$save_rbp = $rbp')
    # In GDB, assignments to sp must be done from the
    # top-most frame, so select frame 0 first.
    gdb.execute('select-frame 0')
    gdb.parse_and_eval('$sp = {0}'.format(str(regs.rsp)))
    gdb.parse_and_eval('$pc = {0}'.format(str(regs.rip)))
    gdb.parse_and_eval('$rbp = {0}'.format(str(regs.rbp)))
    return saved_frame

def restore_context(saved_frame: Optional[gdb.Frame] = None):
    gdb.execute('select-frame 0')
    gdb.parse_and_eval('$pc = $save_pc')
    gdb.parse_and_eval('$sp = $save_sp')
    gdb.parse_and_eval('$rbp = $save_rbp')
    if saved_frame:
        saved_frame.select()

class SwitchContextMICmd(gdb.MICommand):
    def __init__(self) -> None:
        super(SwitchContextMICmd, self).__init__(
            "-sctx"
        )

    def invoke(self, args):
        global saved_frame
        if len(args) != 3:
            print("Usage: -sctx rip rsp rbp")
            return
        rip, rsp, rbp = args[0], args[1], args[2]
        saved_frame = switch_context(Registers(rip, rsp, rbp))

class SwitchContextCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "sctx", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, _arg, _from_tty):
        global sctx_mi_cmd
        argv = gdb.string_to_argv(_arg)
        sctx_mi_cmd.invoke(argv)

class RestoreContextMICmd(gdb.MICommand):
    def __init__(self) -> None:
        super(RestoreContextMICmd, self).__init__(
            "-rctx"
        )

    def invoke(self, args):
        global saved_frame
        restore_context(saved_frame)

class RestoreContextCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "rctx", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

    def invoke(self, _arg, _from_tty):
        global rctx_mi_cmd
        rctx_mi_cmd.invoke(None)
        print("executed rctx")


class DistributedBacktraceInContextMICmd(gdb.MICommand):
    def __init__(self):
        super(DistributedBacktraceInContextMICmd, self).__init__(
            "-stack-list-distri-frames-ctx")
        self.dbt_mi_cmd = dbt_mi_cmd

    def invoke(self, args):
        rip, rsp, rbp = args[0], args[1], args[2]
        saved_frame = switch_context(Registers(rip, rsp, rbp))

        # gdb.execute_mi("-stack-list-frames")
        tracestack = self.dbt_mi_cmd.invoke(None)
        restore_context(saved_frame)
        return tracestack 

class ShowCaladanThreadCmd(gdb.Command):
    "List all caladan threads."

    def __init__(self):
        gdb.Command.__init__(
            self, "info cldths",
            gdb.COMMAND_STACK, gdb.COMPLETE_NONE
        )

    def invoke(self, _arg, _from_tty):
        # args = gdb.string_to_argv(arg)
        count = 0
        saw_ptr = []
        vp = gdb.lookup_type('void').pointer()
        ks = gdb.parse_and_eval("ks")
        lb, up = ks.type.range()
        for i in range(lb, up + 1):
            ks_ptr = ks[i]
            if ks_ptr == 0 or ks_ptr in saw_ptr:
                continue
            else:
                saw_ptr.append(ks_ptr)
                th = ks_ptr.dereference()
                idx = int(th["kthread_idx"])
                # print(f"\nkth: {th}; kthread_idx: {idx}; index: {i}")
                rq = th["rq"]
                rq_lb, rq_up = rq.type.range()
                for j in range(rq_lb, rq_up + 1):
                    cldth_ptr = rq[j]
                    if cldth_ptr == 0 or cldth_ptr in saw_ptr:
                        continue
                    else:
                        saw_ptr.append(cldth_ptr)
                        cldth = cldth_ptr.dereference()
                        if cldth["nu_state"]["owner_proclet"] != 0:
                            print(f"kthread idx: {idx}; cldth idx: {count}")
                            print(f"\tptr: {cldth_ptr}")
                            print(f"\t{cldth}")
                            count += 1
            # print(cldth)

        # for kth in gdb.parse_and_eval("ks").type.range():
        # 	for cldth in kth["rq"].reference_value():
        # 		print(cldth)
            # if ptr['atomicstatus']['value'] == G_DEAD:
            # 	continue
            # s = ' '
            # if ptr['m']:
            # 	s = '*'
            # pc = ptr['sched']['pc'].cast(vp)
            # pc = pc_to_int(pc)
            # blk = gdb.block_for_pc(pc)
            # status = int(ptr['atomicstatus']['value'])
            # st = sts.get(status, "unknown(%d)" % status)
            # print(s, ptr['goid'], "{0:8s}".format(st), blk.function)


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

# def find_goroutine(goid):
# 	"""
# 	find_goroutine attempts to find the goroutine identified by goid.
# 	It returns a tuple of gdb.Value's representing the stack pointer
# 	and program counter pointer for the goroutine.

# 	@param int goid

# 	@return tuple (gdb.Value, gdb.Value)
# 	"""
# 	vp = gdb.lookup_type('void').pointer()
# 	for ptr in SliceValue(gdb.parse_and_eval("'runtime.allgs'")):
# 		if ptr['atomicstatus']['value'] == G_DEAD:
# 			continue
# 		if ptr['goid'] == goid:
# 			break
# 	else:
# 		return None, None
# 	# Get the goroutine's saved state.
# 	pc, sp = ptr['sched']['pc'], ptr['sched']['sp']
# 	status = ptr['atomicstatus']['value']&~G_SCAN
# 	# Goroutine is not running nor in syscall, so use the info in goroutine
# 	if status != G_RUNNING and status != G_SYSCALL:
# 		return pc.cast(vp), sp.cast(vp)

# 	# If the goroutine is in a syscall, use syscallpc/sp.
# 	pc, sp = ptr['syscallpc'], ptr['syscallsp']
# 	if sp != 0:
# 		return pc.cast(vp), sp.cast(vp)
# 	# Otherwise, the goroutine is running, so it doesn't have
# 	# saved scheduler state. Find G's OS thread.
# 	m = ptr['m']
# 	if m == 0:
# 		return None, None
# 	for thr in gdb.selected_inferior().threads():
# 		if thr.ptid[1] == m['procid']:
# 			break
# 	else:
# 		return None, None
# 	# Get scheduler state from the G's OS thread state.
# 	curthr = gdb.selected_thread()
# 	try:
# 		thr.switch()
# 		pc = gdb.parse_and_eval('$pc')
# 		sp = gdb.parse_and_eval('$sp')
# 	finally:
# 		curthr.switch()
# 	return pc.cast(vp), sp.cast(vp)


# class CaladanThreadCmd(gdb.Command):
# 	"""Execute gdb command in the context of goroutine <goid>.

# 	Switch PC and SP to the ones in the goroutine's G structure,
# 	execute an arbitrary gdb command, and restore PC and SP.

# 	Usage: (gdb) goroutine <goid> <gdbcmd>

# 	You could pass "all" as <goid> to apply <gdbcmd> to all goroutines.

# 	For example: (gdb) goroutine all <gdbcmd>

# 	Note that it is ill-defined to modify state in the context of a goroutine.
# 	Restrict yourself to inspecting values.
# 	"""

# 	def __init__(self):
# 		gdb.Command.__init__(self, "cldth", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)

# 	def invoke(self, arg, _from_tty):
# 		goid_str, cmd = arg.split(None, 1)
# 		goids = []

# 		if goid_str == 'all':
# 			for ptr in SliceValue(gdb.parse_and_eval("'runtime.allgs'")):
# 				goids.append(int(ptr['goid']))
# 		else:
# 			goids = [int(gdb.parse_and_eval(goid_str))]

# 		for goid in goids:
# 			self.invoke_per_goid(goid, cmd)

# 	def invoke_per_goid(self, goid, cmd):
# 		pc, sp = find_goroutine(goid)
# 		if not pc:
# 			print("No such goroutine: ", goid)
# 			return
# 		pc = pc_to_int(pc)
# 		save_frame = gdb.selected_frame()
# 		gdb.parse_and_eval('$save_sp = $sp')
# 		gdb.parse_and_eval('$save_pc = $pc')
# 		# In GDB, assignments to sp must be done from the
# 		# top-most frame, so select frame 0 first.
# 		gdb.execute('select-frame 0')
# 		gdb.parse_and_eval('$sp = {0}'.format(str(sp)))
# 		gdb.parse_and_eval('$pc = {0}'.format(str(pc)))
# 		try:
# 			gdb.execute(cmd)
# 		finally:
# 			# In GDB, assignments to sp must be done from the
# 			# top-most frame, so select frame 0 first.
# 			gdb.execute('select-frame 0')
# 			gdb.parse_and_eval('$pc = $save_pc')
# 			gdb.parse_and_eval('$sp = $save_sp')
# 			save_frame.select()

class MIEcho(gdb.MICommand):
    """Echo arguments passed to the command."""

    def __init__(self, name, mode):
        self._mode = mode
        super(MIEcho, self).__init__(name)

    def invoke(self, argv):
        if self._mode == 'dict':
            return {'dict': {'argv': argv}}
        elif self._mode == 'list':
            return {'list': argv}
        else:
            return {'string': ", ".join(argv)}


MIEcho("-echo-dict", "dict")
MIEcho("-echo-list", "list")
MIEcho("-echo-string", "string")

GetGlobalVarCommand()
dbt_mi_cmd = DistributedBacktraceMICmd()
DistributedBacktraceInContextMICmd()
dbt_cmd = DistributedBTCmd()

sctx_mi_cmd = SwitchContextMICmd()
sctx_cmd = SwitchContextCmd()
rctx_mi_cmd = RestoreContextMICmd()
rctx_cmd = RestoreContextCmd()

ShowCaladanThreadCmd()
