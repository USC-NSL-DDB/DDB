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
        super().__init__("-get-lock-state")

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

def get_global_variable(var_name, to_print: bool = False, check_is_var: bool = True) -> gdb.Value:
    try:
        var = gdb.lookup_symbol(var_name)[0]
        # print(f"type: {var.type}")
        # print(f"symtab: {var.symtab}")
        # print(f"addr_class: {var.addr_class}")
        # print(f"is_const: {var.is_constant}")
        # print(f"is_var: {var.is_variable}")
        # print(f"is_function: {var.is_function}")
        # print(f"is_argument: {var.is_argument}")
        # print(f"linkage_name: {var.linkage_name}")
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

print("Loaded extension.py")
# GetGlobalVarCommand()
get_lock_state_cmd_mi = GetLockStateMI()
get_lock_state_cmd =GetLockState()
# dbt_mi_cmd = DistributedBacktraceMICmd()
# dbt_cmd = DistributedBTCmd()
get_thrd_ktid_cmd_mi = GetThreadKtidMI()
GetThreadKtid()
