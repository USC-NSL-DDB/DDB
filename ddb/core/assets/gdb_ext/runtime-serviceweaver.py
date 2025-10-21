from dataclasses import dataclass
from enum import Enum
import platform
import re
import time
import gdb
import sys
from typing import ClassVar, List, Dict, Union
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

import logging
def setup_logger(name='app', log_file='app.log', level=logging.INFO):
    """
    Setup a basic logger that writes to both file and console.
    The log file is overwritten each time the program runs.
    
    Args:
        name (str): Logger name
        log_file (str): Path to log file
        level: Logging level (e.g., logging.INFO, logging.DEBUG)
    
    Returns:
        logging.Logger: Configured logger instance
    """
    # Create logger
    logger = logging.getLogger(name)
    logger.setLevel(level)
    
    # Create formatters and handlers
    formatter = logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
    
    # File handler (use filemode='w' to overwrite the file each time)
    file_handler = logging.FileHandler(log_file, mode='w')
    file_handler.setFormatter(formatter)
    
    # Console handler
    console_handler = logging.StreamHandler()
    console_handler.setFormatter(formatter)
    
    # Add handlers to logger
    logger.addHandler(file_handler)
    # logger.addHandler(console_handler)
    
    return logger
logger=setup_logger("command_history","command_history.log",logging.DEBUG)
class GetRemoteBTInfo(gdb.MICommand):
    def __init__(self):
        super().__init__("-serviceweaver-bt-remote")

    def invoke(self, args):
        # sid,token = args[0],args[1]
        # logger.debug(f"sid: {sid}, token: {token}, start_time: {time.time_ns()}")
        try:
            print("before selected frame")
            frame = gdb.selected_frame()
            frames: List[gdb.Frame] = []
            while frame is not None and frame.is_valid():
                frames.append(frame)
                frame = frame.older()
            ip_address = 0
            port = -1
            parent_rsp = -1
            parent_rip = -1
            message = "not found"
            print("before iterating frames")
            for cur_frame in frames:
                if cur_frame.function() is not None and cur_frame.function().name.endswith("runHandler"):
                    # print("found")
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

                                # print(f"parent_rsp: {parent_rsp}")
                                # print(f"parent_rip {parent_rip}")
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
                                # print(f"ip_address: {ip_address}") 
                                ip_int = (ip_address[-4] << 24) + (ip_address[-3] << 16) + (ip_address[-2] << 8) + ip_address[-1]
                                # print(f"ip_int: {ip_int}")
                                message = "success"
                                ip_address = ip_int
                                 
        except Exception as e: 
            print(e)
            message = "error"
        finally:
            # logger.debug(f"sid: {sid}, token: {token}, end_time: {time.time_ns()}")
            return {
            "message": message,
            "metadata": {
                "caller_ctx": {
                    "pc": parent_rip,
                    "sp": parent_rsp,
                },
                "caller_meta": {
                    "ip": ip_address
                },
                "local_meta": {
                }
            }
        }
@dataclass(frozen=True)
class Architecture:
    name: str
    register_map: Dict[str, str]
    
    # Class-level constants for register aliases
    PC: ClassVar[str] = "pc"
    SP: ClassVar[str] = "sp" 
    FP: ClassVar[str] = "fp"
    LR: ClassVar[str] = "lr"

    # Architecture definitions
    X86_64 = None  # Will be set below
    AARCH64 = None

Architecture.X86_64 = Architecture(
    name="x86_64",
    register_map={
        Architecture.PC: "rip",
        Architecture.SP: "rsp", 
        Architecture.FP: "rbp"
    }
)

Architecture.AARCH64 = Architecture(
    name="aarch64",
    register_map={
        Architecture.PC: "pc",
        Architecture.SP: "sp",
        Architecture.FP: "x29",
        Architecture.LR: "lr"
    }
)

def get_architecture() -> Architecture:
    arch = platform.machine()
    if arch == 'x86_64':
        return Architecture.X86_64
    elif arch in ('aarch64', 'arm64'):
        return Architecture.AARCH64
    else:
        raise ValueError(f"Unsupported architecture: {arch}")
class SwitchContextMICmd(gdb.MICommand):
    def __init__(self) -> None:
        super(SwitchContextMICmd, self).__init__(
            "-switch-context-custom"
        )

    def invoke(self, args):
        token = self.token if hasattr(self, 'token') else None
        print(f"Command token: {token}")
        try:
            # validate args has correct names
            valid_aliases = [Architecture.PC, Architecture.SP, Architecture.FP, Architecture.LR]
            reg_pairs = []
            for arg in args:
                splits = arg.split("=")
                if len(splits) != 2:
                    raise ValueError(f"Invalid argument: {arg}")
                reg_alias, val = splits
                if reg_alias not in valid_aliases:
                    raise ValueError(f"Invalid register alias: {reg_alias}")
                num_val = int(val, 0)
                reg_pairs.append((reg_alias, num_val))
            
            arch = get_architecture()
            print("reg_to_set: ", reg_pairs)
            
            old_ctx: Dict[str, int] = {}
            gdb.execute('select-frame 0')
            
            # Fixed loop
            for reg_alias, num_val in reg_pairs:
                try:
                    reg_real = arch.register_map[reg_alias]
                    # extract the current value for that register
                    reg_val_to_save = int(gdb.parse_and_eval(f'${reg_real}'))
                    # save it to the old context with register alias name
                    old_ctx[str(reg_alias)] = reg_val_to_save
                except KeyError:
                    continue
                # Use num_val instead of undefined val
                gdb.parse_and_eval(f'${reg_real} = {num_val}')
                print(f"set {reg_real} ({reg_alias}) to {num_val}. old = {reg_val_to_save}")
                
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
            
class RecordTimeAndContinueMiCommand(gdb.MICommand):
    def __init__(self):
        super(RecordTimeAndContinueMiCommand, self).__init__(
            "-record-time-and-continue")

    def invoke(self, args):
        global pause_start_time, accumulated_time,cont_time
        # if len(args) < 2:
        #     return {"message": "error", "error": "missing arguments"}
        # pause_start_time=float(args[0])
        # accumulated_time=float(args[1])
        # print(f"pause_start_time:{pause_start_time}, accumulated_time:{accumulated_time}")
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
            # setenv_command = f'setenv("FAKETIME", "-{accumulated_time}", 1)'
            # gdb.execute(f'call (int){setenv_command}', to_string=True)
            # safe_setenv("FAKETIME", f"-{accumulated_time}", 1)
            # time.sleep(0.5)
            cont_time = time.perf_counter_ns()
            gdb.execute("continue")
        except Exception as e:
            return {"message": "error", "error": str(e)}
        return {"message": "success", "paused_time": accumulated_time} 
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

GetRemoteBTInfo()
RecordTimeAndContinueMiCommand()
SwitchContextMICmd()