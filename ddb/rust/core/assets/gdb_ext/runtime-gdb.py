import time
from typing import Dict, List, Optional, Callable
import socket
import struct
import platform
import sys
from enum import Enum

import gdb

# try:
#     import debugpy
# except ImportError:
#     print("Failed to import debugpy")

# try:
#     import random
#     # port = random.randint(5700, 5800)
#     port = 5800
#     debugpy.listen(("localhost", port))
#     print(f"Waiting for debugger attach: {port}")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")

print("Loading DDB support.",)

class Arch(Enum):
    X86_64 = "x86_64"
    AARCH64 = "aarch64"

class Reg(Enum):
    PC = "pc"
    SP = "sp"
    FP = "fp"
    LR = "lr" # only for AARCH64

    def __str__(self) -> str:
        return self.value

REGISTER_MAP = {
    Arch.X86_64: {
        Reg.PC: "rip",
        Reg.SP: "rsp",
        Reg.FP: "rbp"
    },
    Arch.AARCH64: {
        Reg.PC: "pc",
        Reg.SP: "sp",
        Reg.FP: "x29",
        Reg.LR: "lr"
    },
}

def get_architecture() -> Arch:
    arch = platform.machine()
    if arch == 'x86_64':
        return Arch.X86_64
    elif arch in ('aarch64', 'arm64'):
        return Arch.AARCH64
    else:
        raise ValueError(f"Unsupported architecture: {arch}")

class DistributedBTCmd(gdb.Command):
    def __init__(self):
        gdb.Command.__init__(self, "dbt", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        # gdb.Command.__init__(self, "dbacktrace", gdb.COMMAND_STACK, gdb.COMPLETE_NONE)
        self.mi_cmd = dbt_mi_cmd

    def invoke(self, _arg, _from_tty):
        result = self.mi_cmd.invoke(None)
        if "stack" in result:
            stacks = result["stack"]
            for stack in stacks:
                filepath = stack['file'] if 'file' in stack else ""
                print(f"{stack['level']} {stack['func']} file:{filepath}") 
        else:
            print("no stack info presented")


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
        local_ip: Optional[int] = None
        proclet_id: Optional[int] = None
        parent_rip: Optional[int] = None
        parent_rsp: Optional[int] = None
        parent_rbp: Optional[int] = None
        pid: Optional[int] = None

        is_remote_call = False

        for cur_frame in frames:
            curr_func = cur_frame.function()
            if curr_func and curr_func.name.startswith("DDB::Backtrace::extraction"):
                is_remote_call = True
                for sym in get_local_variables(cur_frame):
                    if sym.name == "meta":
                        val = sym.value(cur_frame)
                        remote_ip = int(val['meta']['caller_comm_ip'])
                        pid = int(val['meta']['pid'])
                        parent_rip = int(val['ctx']['rip'])
                        parent_rsp = int(val['ctx']['rsp'])
                        parent_rbp = int(val['ctx']['rbp'])
                        break
            if is_remote_call:
                break
        print(f"ip: {remote_ip}, pid: {pid}, rip: {parent_rip}, rsp: {parent_rsp}, rbp: {parent_rbp}")

        if not is_remote_call:
            return result
        
        ddb_meta = get_global_variable(
            "ddb_meta", to_print=False, check_is_var=False)
        if ddb_meta:
            local_ip = int(ddb_meta["comm_ip"])
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
            "-switch-context-custom"
        )

    def invoke(self, args):
        try:
            reg_map = REGISTER_MAP[get_architecture()]
            reg_to_set = map(lambda reg_pair: tuple(reg_pair.split("=")), args)
            old_ctx: Dict[str, int] = {}
            gdb.execute('select-frame 0')
            for (reg_alias, val) in reg_to_set:
                try:
                    reg_real = reg_map[Reg(reg_alias)]
                    # extract the current value for that register.
                    reg_val_to_save = int(gdb.parse_and_eval(f'${reg_real}'))
                    # save it to the old context with register alias name.
                    old_ctx[str(reg_alias)] = reg_val_to_save
                except KeyError:
                    continue
                gdb.parse_and_eval(f'${reg_real} = {val}')
            return {
                "message": "success",
                "old_ctx": old_ctx
            }
        except Exception as e:
            return {
                "message": "error",
                "old_ctx": {}
            }

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

class GetRemoteBTInfo(gdb.MICommand):
    def __init__(self):
        super().__init__("-get-remote-bt")

    def invoke(self, argv):
        remote_ip: Optional[int] = -1
        proclet_id: Optional[int] = 0
        regs: Dict[str, int] = {}
        pid: Optional[int] = -1
        tid: Optional[int] = -1
        frame = gdb.selected_frame()
        frames: List[gdb.Frame] = []
        message = "failed"
        found = False
        try:
            while frame is not None and frame.is_valid():
                    frames.append(frame)
                    frame = frame.older()
            for cur_frame in frames:
                if found: 
                    break
                curr_func = cur_frame.function()
                if curr_func and curr_func.name.startswith("DDB::Backtrace::extraction"):
                    for sym in get_local_variables(cur_frame):
                        if sym.name == "meta":
                            val = sym.value(cur_frame)
                            meta = val['meta']
                            remote_ip = int(meta['caller_comm_ip'])
                            if "proclet_id" in meta:
                                proclet_id = int(meta['proclet_id'])
                            pid = int(meta['pid'])
                            tid = int(meta['tid'])
                            ctx_obj = val['ctx']
                            if ctx_obj.type.code == gdb.TYPE_CODE_STRUCT:
                                for field in ctx_obj.type.fields():
                                    fname = field.name
                                    fval = ctx_obj[fname]
                                    try:
                                        regs[fname] = int(fval)
                                    except Exception as e:
                                        print(f"failed to convert {fname} (val = {fval}) to int")
                            else:
                                # ERROR
                                print(f"ctx is not a struct, but {ctx_obj.type}")
                                break
                            message = "success"
                            found = True
                            break
            str_to_print = ""
            for (reg, val) in regs.items():
                str_to_print += f"{reg}: {val}, "
            print(f"extracted meta: {str_to_print}")
            # print(f"ip: {remote_ip}, pid: {pid}, pc: {parent_pc}, sp: {parent_sp}, fp: {parent_fp}, lr: {parent_lr}")
        except Exception as e:
            pass
        # global get_thrd_ktid_cmd_mi
        # result = get_thrd_ktid_cmd_mi.invoke([])
        result = gdb.execute_mi("-get-thread-ktid")
        if "error" in result:
            print(f"Error: {result['error']}")
        ktid = result["ktid"] if ("ktid" in result) and (result["ktid"]) else -1
        return {
            "message": message,
            "metadata": {
                "caller_ctx": regs,
                "caller_meta": {
                    "pid": pid,
                    "tid": tid,
                    "ip": remote_ip,
                    "proclet_id": proclet_id if proclet_id else 0
                },
                "local_meta": {
                    "tid": ktid
                }
            }
        }

class GetLockStateMI(gdb.MICommand):
    """A custom command to fetch the lock state (MI)"""

    def __init__(self):
        super().__init__("-get-lock-state")

    def invoke(self, argv):
        ddb_shared: gdb.Value = get_global_variable("ddb_shared", to_print=True, check_is_var=False)
        if not ddb_shared:
            print("didn't find ddb_shared")
            return {}

        thread_infos = ddb_shared['ddb_thread_infos']
        lowners = ddb_shared['ddb_lowners']
            
        # Parse thread infos array
        thread_max_count = int(ddb_shared['ddb_max_idx'])
        thread_data = []
        for i in range(thread_max_count):
            info = thread_infos[i]
            if info["valid"]:
                wbuf = info['wbuf']
                wbuf_len = wbuf['max_n']
                wait_entries = wbuf['wait_entries']
                wait_buf = []
                for j in range(wbuf_len):
                    wait_ent = wait_entries[j]
                    if wait_ent['valid']:
                        wait_buf.append({
                            "type": int(wait_ent['type']),
                            "id": int(wait_ent['identifier']),
                        })

                thread_data.append({
                    'tid': int(info['id']),
                    'fsbase': int(info['fsbase']),
                    'stackbase': int(info['stackbase']),
                    'wait': wait_buf
                })

        from pprint import pprint
        # pprint(thread_data)
        # Parse lock states array
        lock_max_count = int(lowners['max_n'])
        lock_data = []
        for i in range(lock_max_count):
            lock = lowners['lowner_entries'][i]
            if lock['valid']:
                lock_data.append({
                    'lid': int(lock['lid']),
                    'owner_tid': int(lock['owner_tid'])
                })
        return {
            "thread_info": thread_data,
            "lock_info": lock_data
        }

        # pprint(lock_data)

class GetLockState(gdb.Command):
    """A custom command to fetch the lock state"""

    def __init__(self):
        super(GetLockState, self).__init__(
            "get-lock-state",
            gdb.COMMAND_DATA,
            gdb.COMPLETE_SYMBOL
        )

    def invoke(self, arg, from_tty):
        from pprint import pprint
        global get_lock_state_cmd_mi
        r = get_lock_state_cmd_mi.invoke(None)
        pprint(r)

def get_thread_tid(thread: gdb.Thread = None):
    """Get kernel thread ID (LWP) for a given thread or current thread"""
    try:
        if thread is None:
            thread = gdb.selected_thread()
        if thread is None:
            return None
            
        # Get thread info string which contains LWP
        thread_info = thread.ptid
        # PTID is a tuple of (pid, lwpid, tid)
        # For Linux threads, lwpid is the kernel thread ID
        return thread_info[1]
    except gdb.error:
        return None

class GetThreadKtid(gdb.Command):
    """Command to print kernel thread ID for current or specified thread
    Usage: thread-tid [thread_num]"""
    
    def __init__(self):
        super(GetThreadKtid, self).__init__("get-thread-ktid", gdb.COMMAND_DATA)
    
    def invoke(self, arg, from_tty):
        global get_thrd_ktid_cmd_mi
        result = get_thrd_ktid_cmd_mi.invoke(arg)
        if "error" in result:
            print(f"Error: {result['error']}")
        else:
            print(f"Kernel thread ID (LWP) for thread {result['thrd_num']}: {result['ktid']}")
       
class GetThreadKtidMI(gdb.MICommand):
    """MI Command to print kernel thread ID for current or specified thread
    Usage: thread-tid [thread_num]"""
    
    def __init__(self):
        super().__init__("-get-thread-ktid")

    def invoke(self, argv):
        result = {
            "ktid": None
        }
        try:
            if argv and argv[0]:
                # If thread number specified, find that thread
                thread_num = int(argv[0])
                thread = None
                for t in gdb.selected_inferior().threads():
                    if t.num == thread_num:
                        thread = t
                        break
                if thread is None:
                    result["error"] = f"Thread {thread_num} not found"
                    return
            else:
                thread = gdb.selected_thread()
                if thread is None:
                    result["error"] = "No thread selected"
                    return
            
            tid = get_thread_tid(thread)
            if tid is not None:
                result["ktid"] = tid
                result["thrd_num"] = thread.num
                # print(f"Kernel thread ID (LWP) for thread {thread.num}: {tid}")
            else:
                result["error"] = "Failed to get kernel thread ID"
        except ValueError:
            result["error"] = "Invalid thread number"
        except gdb.error as e:
            result["error"] = str(e)
            print(f"Error: {str(e)}")
        return result

pause_start_time = 0
accumulated_time = 0
def stop_handler(event:gdb.StopEvent):
    global pause_start_time
    pause_start_time=time.perf_counter_ns()
    # print(f"pause detected, {pause_start_time} {dir(event)} {str(event)} {event.__dict__}")
    print(f"pause detected, {pause_start_time}")

cont_time = 0
def cont_handler(event:gdb.ContinueEvent):
    global cont_time
    print(f"continue detected, {(time.perf_counter_ns() - cont_time) / 1e6} ms")

gdb.events.stop.connect(stop_handler)
gdb.events.cont.connect(cont_handler)

def sync_pause_time(on_finish: Callable[[], None] = None):
    global pause_start_time, accumulated_time,cont_time
    ret = None
    try:
        print(f"timestamp: {time.perf_counter_ns()}")
        paused_time_ns=time.perf_counter_ns()
        start_env_time = time.perf_counter_ns()
        if pause_start_time > paused_time_ns:
            raise Exception("pause_start_time is greater than current time")
        paused_time1=(paused_time_ns-pause_start_time)/ 1e9
        accumulated_time = round(paused_time1 + accumulated_time, 9)
        print(f"paused_time_ns:{paused_time1}, accumulated_time:{accumulated_time}")
        modify_env_variable("FAKETIME", f"-{accumulated_time}")
        print(f"modify_env_variable time: {(time.perf_counter_ns() - start_env_time) / 1e6} ms")
        cont_time = time.perf_counter_ns()
        ret = {"message": "success", "paused_time": accumulated_time} 
    except Exception as e:
        ret = {"message": "error", "error": str(e)}
    finally:
        on_finish() if on_finish else None
        # gdb.execute_mi("-exec-continue")
    return ret
    

class RecordTimeAndContinueMiCommand(gdb.MICommand):
    def __init__(self):
        super(RecordTimeAndContinueMiCommand, self).__init__(
            "-record-time-and-continue")

    def invoke(self, args):
        return sync_pause_time(on_finish=lambda: gdb.execute_mi("-exec-continue", *args))
        # global pause_start_time, accumulated_time,cont_time
        # ret = None
        # try:
        #     print(f"timestamp: {time.perf_counter_ns()}")
        #     paused_time_ns=time.perf_counter_ns()
        #     start_env_time = time.perf_counter_ns()
        #     if pause_start_time > paused_time_ns:
        #         raise Exception("pause_start_time is greater than current time")
        #     paused_time1=(paused_time_ns-pause_start_time)/ 1e9
        #     accumulated_time = round(paused_time1 + accumulated_time, 9)
        #     print(f"paused_time_ns:{paused_time1}, accumulated_time:{accumulated_time}")
        #     modify_env_variable("FAKETIME", f"-{accumulated_time}")
        #     print(f"modify_env_variable time: {(time.perf_counter_ns() - start_env_time) / 1e6} ms")
        #     cont_time = time.perf_counter_ns()
        #     ret = {"message": "success", "paused_time": accumulated_time} 
        # except Exception as e:
        #     ret = {"message": "error", "error": str(e)}
        # finally:
        #     gdb.execute_mi("-exec-continue")
        # return ret
    
class RecordTimeAndNextMiCommand(gdb.MICommand):
    def __init__(self):
        super(RecordTimeAndNextMiCommand, self).__init__(
            "-record-time-and-next")
        
    def invoke(self, args):
        return sync_pause_time(on_finish=lambda: gdb.execute_mi("-exec-next", *args))

class RecordTimeAndStepMiCommand(gdb.MICommand):
    def __init__(self):
        super(RecordTimeAndStepMiCommand, self).__init__(
            "-record-time-and-step")
        
    def invoke(self, args):
        return sync_pause_time(on_finish=lambda: gdb.execute_mi("-exec-step", *args))

class RecordTimeAndFinishMiCommand(gdb.MICommand):
    def __init__(self):
        super(RecordTimeAndFinishMiCommand, self).__init__(
            "-record-time-and-finish")
        
    def invoke(self, args):
        return sync_pause_time(on_finish=lambda: gdb.execute_mi("-exec-finish", *args))

def find_environ_ptr():
    try:
        # Try direct symbol access first
        try:
            environ_addr = gdb.parse_and_eval('(char**)environ')
            if environ_addr != 0:
                return environ_addr
        except:
            pass

        # Fallback to parsing info variables
        symbols = gdb.execute('info variables environ', to_string=True)
        for line in symbols.split('\n'):
            if line.strip().endswith(' environ'):
                try:
                    addr_str = line.split()[0]
                    return gdb.Value(int(addr_str, 16))
                except:
                    continue
        return None
        
    except Exception as e:
        print(f"Error finding environ: {e}")
        return None

def modify_env_variable(env_name, new_value):
    # Get environ pointer
    environ_ptr = find_environ_ptr()
    if not environ_ptr:
        print("Could not find environ pointer")
        return False

    try:
        # Get char* type for proper casting
        char_ptr_t = gdb.lookup_type('char').pointer()
        char_ptr_ptr_t = char_ptr_t.pointer()
        
        # Cast environ pointer to char**
        environ_ptr = environ_ptr.cast(char_ptr_ptr_t)
        
        idx = 0
        while True:
            try:
                # Get pointer to current environment string
                env_str_ptr = environ_ptr[idx]
                
                # Check for end of environ array
                if int(env_str_ptr) == 0:
                    break
                
                # Convert to string safely
                env_str = env_str_ptr.string()
                
                if env_str.startswith(f"{env_name}="):
                    # Create new string
                    new_env_str = f"{env_name}={new_value}"
                    
                    # Get the buffer address
                    buffer_addr = int(env_str_ptr)
                    
                    # Write the new string directly to the existing buffer
                    for i, c in enumerate(new_env_str):
                        gdb.execute(f"set *(char*)({buffer_addr + i}) = {ord(c)}")
                    # Null terminate
                    gdb.execute(f"set *(char*)({buffer_addr + len(new_env_str)}) = 0")
                    
                    return True
                
                idx += 1
                
            except gdb.MemoryError:
                print(f"Memory error at index {idx}")
                break
            except Exception as e:
                print(f"Error: {e}")
                break
                
        return False
        
    except Exception as e:
        print(f"Error modifying environment variable: {e}")
        return False

GetGlobalVarCommand()

get_lock_state_cmd_mi = GetLockStateMI()
get_lock_state_cmd =GetLockState()

get_thrd_ktid_cmd_mi = GetThreadKtidMI()
get_thrd_ktid_cmd = GetThreadKtid()

dbt_mi_cmd = DistributedBacktraceMICmd()
DistributedBacktraceInContextMICmd()
dbt_cmd = DistributedBTCmd()

sctx_mi_cmd = SwitchContextMICmd()
sctx_cmd = SwitchContextCmd()
rctx_mi_cmd = RestoreContextMICmd()
rctx_cmd = RestoreContextCmd()
GetRemoteBTInfo()
ShowCaladanThreadCmd()

RecordTimeAndContinueMiCommand()
RecordTimeAndNextMiCommand()
RecordTimeAndStepMiCommand()
RecordTimeAndFinishMiCommand()
