from typing import Dict, List, Optional
import socket
import struct
import platform
import sys
from enum import Enum

import gdb

try:
    import debugpy
except ImportError:
    print("Failed to import debugpy")

# try:
#     debugpy.listen(("localhost", 5680))
#     print("Waiting for debugger attach")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")
# print("Loading distributed backtrace support.", file=sys.stderr)

# allow to manually reload while developing
# goobjfile = gdb.current_objfile() or gdb.objfiles()[0]
# goobjfile.pretty_printers = []
print("Loading distributed backtrace support.",)

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
        # handle_invoke()
        # result = gdb.execute_mi("-stack-list-distributed-frames")
        # print(f"result:\n{result}")
        # print(gdb.execute("bt"))
        result = self.mi_cmd.invoke(None)
        
        if "stack" in result:
            stacks = result["stack"]
            for stack in stacks:
                filepath = stack['file'] if 'file' in stack else ""
                print(f"{stack['level']} {stack['func']} file:{filepath}") 
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
            if curr_func and curr_func.name.startswith("DDB::Backtrace::extraction"):
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
            "-switch-context-custom"
        )

    # def invoke(self, args):
    #     print("invoke 1")
    #     try:
    #         cur_rip, cur_rsp = map(int, args[:2])
    #         cur_rbp = int(args[2]) if len(args) > 2 else None
            
    #         # Save current register values
    #         for reg in ['sp', 'pc', 'rbp']:
    #             gdb.parse_and_eval(f'$save_{reg} = ${reg}')
            
    #         gdb.execute('select-frame 0')
            
    #         # Set new register values
    #         gdb.parse_and_eval(f'$sp = {cur_rsp}')
    #         gdb.parse_and_eval(f'$pc = {cur_rip}')
    #         if cur_rbp is not None:
    #             gdb.parse_and_eval(f'$rbp = {cur_rbp}')
            
    #         # Store original values
    #         original_values = {reg: int(gdb.parse_and_eval(f'$save_{reg}')) 
    #                            for reg in ['pc', 'sp', 'rbp']}
            
    #         return {"message": "success", **original_values}
    #     except Exception as e:
    #         return {"message": "error", "rip": None, "rsp": None, "rbp": None}

    def invoke(self, args):
        try:
            reg_map = REGISTER_MAP[get_architecture()]

            reg_to_set = map(lambda reg_pair: tuple(reg_pair.split("=")), args)
            print("reg_to_set: ", reg_to_set)

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
                print(f"set {reg_real} ({reg_alias}) to {val}. old = {reg_val_to_save}")

            print(f"old ctx: {old_ctx}")
            # for (reg_alias, reg_real) in reg_map.items():
            #     if (str(reg_alias) == )
                # gdb.parse_and_eval(f'${reg} = {val}')
                
            # cur_rip, cur_rsp, cur_rbp = map(int, args[:3])

            # # Save current register values
            # for reg in ['sp', 'pc', 'rbp']:
            #     gdb.parse_and_eval(f'$save_{reg} = ${reg}')
            
            
            # # Set new register values
            # for reg, value in zip(['sp', 'pc', 'rbp'], [cur_rsp, cur_rip, cur_rbp]):
            #     gdb.parse_and_eval(f'${reg} = {value}')
            
            # # Store original values
            # original_values = {reg: int(gdb.parse_and_eval(f'$save_{reg}')) 
            #                 for reg in ['sp', 'pc', 'rbp']}
            
            # return {"message": "success", "rip":original_values['pc'], "rsp":original_values['sp'], "rbp":original_values['rbp']}
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

class GetRemoteBTInfo(gdb.MICommand):
    def __init__(self):
        super().__init__("-get-remote-bt")

    def invoke(self, argv):
        remote_ip: Optional[int] = -1
        local_ip: Optional[int] = -1
        # parent_pc: Optional[int] = -1
        # parent_sp: Optional[int] = -1
        # parent_fp: Optional[int] = -1
        # parent_lr: Optional[int] = None
        regs: Dict[str, int] = {}
        pid: Optional[int] = -1
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
                            # print("found meta")
                            val = sym.value(cur_frame)
                            # print("found val")
                            remote_ip = int(val['meta']['caller_comm_ip'])
                            # print("found ip")
                            pid = int(val['meta']['pid'])
                            # print("found pid")
                            # parent_pc = int(val['ctx']['pc'])
                            # parent_sp = int(val['ctx']['sp'])
                            # parent_fp = int(val['ctx']['fp'])
                            # parent_lr = int(val['ctx']['lr']) if 'lr' in val['ctx'] else None # only available on AARCH64
                            ctx_obj = val['ctx']
                            # ctx_map: Dict[str, int] = {}
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
                                    
                            # regs = { str(reg): int(val) for (reg, val) in val['ctx'].items() }
                            # ddb_meta = get_global_variable(
                            #     "ddb_meta", to_print=False, check_is_var=False)
                            # if ddb_meta:
                            #     local_ip = int(ddb_meta["comm_ip"])
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
        return {
            "message":
                message,
            "metadata": {
                # "callee_meta": {
                #     "ip": local_ip,
                # },
                # "caller_meta": {
                #     "rip": parent_rip,
                #     "rsp": parent_rsp,
                #     "rbp": parent_rbp,
                #     "pid": pid,
                #     "ip": remote_ip
                # }
                "caller_ctx": regs,
                "caller_meta": {
                    "pid": pid,
                    "ip": remote_ip
                }
            }}

# MIEcho("-echo-dict", "dict")
# MIEcho("-echo-list", "list")
# MIEcho("-echo-string", "string")

GetGlobalVarCommand()
dbt_mi_cmd = DistributedBacktraceMICmd()
DistributedBacktraceInContextMICmd()
dbt_cmd = DistributedBTCmd()

sctx_mi_cmd = SwitchContextMICmd()
sctx_cmd = SwitchContextCmd()
rctx_mi_cmd = RestoreContextMICmd()
rctx_cmd = RestoreContextCmd()
GetRemoteBTInfo()
ShowCaladanThreadCmd()
