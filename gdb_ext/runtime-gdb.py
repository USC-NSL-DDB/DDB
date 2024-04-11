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
		self.mi_cmd.invoke(None)
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

class DistributedBacktraceMICmd(gdb.MICommand):
	def __init__(self):
		super(DistributedBacktraceMICmd, self).__init__(
			"-stack-list-distributed-frames")

	def invoke(self, argv):
		from pprint import pprint
		result = gdb.execute_mi("-stack-list-frames")

		frame = gdb.selected_frame()
		frames: List[gdb.Frame]= []
		while frame and frame.pc():
			frames.append(frame)
			frame = frame.older()

		remote_ip: Optional[int] = None
		remote_port: Optional[int] = None
		local_ip: Optional[int] = None
		local_port: Optional[int] = None
		parent_rip: Optional[int]= None
		parent_rsp: Optional[int] = None

		for cur_frame in frames:
			if cur_frame.function().name.startswith("nu::RPCServer::handler_fn"):
				print("found")
				for sym in get_local_variables(cur_frame):
					if sym.name == "meta":
						val = sym.value(cur_frame)
						parent_rip = int(val['rip'])
						parent_rsp = int(val['rsp'])
						print(f"rip: {parent_rip:#x}")
						print(f"rsp: {parent_rsp:#x}")
					if sym.name == "local_addr":
						val = sym.value(cur_frame)
						local_ip = int(val['ip'])
						local_port = int(val['port'])
						print(f"local_addr: ip: {int_to_ip(local_ip)}; port: {local_port}")
					if sym.name == "remote_addr":
						val = sym.value(cur_frame)
						remote_ip = int(val['ip'])
						remote_port = int(val['port'])
						print(f"remote_addr: ip: {int_to_ip(remote_ip)}; port: {remote_port}")

		if remote_ip is None or remote_port is None or local_ip is None or local_port is None:
			print("Failed to find remote/local address/port")
			return result

		if parent_rip is None or parent_rsp is None:
			print("Failed to find parent rip/rsp")
			return result

		backtrace_meta = {
			"remote_addr": {
				"ip": remote_ip,
				"port": remote_port
			},
			"local_addr": {
				"ip": local_ip,
				"port": local_port
			},
			"caller_meta": {
				"rip": parent_rip,
				"rsp": parent_rsp
			}
		}
		result["bt_meta"] = backtrace_meta
		pprint(result)
		return result

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

dbt_mi_cmd = DistributedBacktraceMICmd()
dbt_cmd = DistributedBTCmd()
ShowCaladanThreadCmd()
