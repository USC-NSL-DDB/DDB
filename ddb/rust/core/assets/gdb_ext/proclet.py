from typing import Optional
import binascii
import gdb  # type: ignore
import traceback  # For detailed error reporting

_THREAD_NAME = "DDB_AUX_THREAD"
_FRAME_NAME = "DDB::ddb_aux_thread_main_func"
_PROCLET_TYPE = "nu::ProcletHeader"
# _ABSENT_STATUS_NAME = "nu::kAbsent"

# try:
#     import debugpy
# except ImportError:
#     print("Failed to import debugpy")

# try:
#     # import random
#     # port = random.randint(5700, 5800)
#     port = 5800
#     debugpy.listen(("localhost", port))
#     print(f"Waiting for debugger attach: {port}")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")


class ThreadInfo:
    def __init__(self, thread: gdb.InferiorThread, frame: gdb.Frame):
        self.thread = thread
        self.frame = frame

    def __str__(self):
        return f"ThreadInfo(thread={self.thread}, frame={self.frame})"

    def switch(self) -> bool:
        # switch thread
        if self.thread.is_valid():
            try:
                if gdb.selected_thread() != self.thread:
                    self.thread.switch()
            except gdb.error as e:
                print("Restore Thread", "Failure", f"Error switching: {e}")
                return False
        else:
            print("Restore Thread", "Skipped", "The thread is invalid.")
            return False

        # switch frame (if valid and thread is correct)
        # Note: Frame selection might implicitly switch threads if not careful.
        if self.thread.is_valid() and gdb.selected_thread() == self.thread:
            try:
                if (
                    gdb.selected_frame().level != self.frame.level
                ):  # Avoid unnecessary select
                    self.frame.select()
            except gdb.error as e:
                print(
                    "Restore Frame", "Failure", f"Error selecting original frame: {e}"
                )
                return False
        else:
            print(
                "Restore Frame",
                "Error",
                "Thread is either invalid or not correctly restored when restoring the frame.",
            )
            return False


def _switch_to_thread(
    thread_name: str, frame_name: Optional[str]
) -> Optional[ThreadInfo]:
    original_thread = gdb.selected_thread()
    original_frame = gdb.selected_frame()
    ret = ThreadInfo(original_thread, original_frame)

    # --- Checkout to the target thread ---
    target_thread: gdb.InferiorThread = None
    try:
        if not gdb.selected_inferior() or not gdb.selected_inferior().threads():
            return None

        for thread in gdb.selected_inferior().threads():
            print(f"Thread: {thread.name}")
            if thread.name == thread_name:
                target_thread = thread
                break

        if target_thread:
            target_thread.switch()
        else:
            msg = f"Thread '{thread_name}' not found."
            print(msg)
            return ret
    except gdb.error as e:
        msg = f"Error finding/switching thread: {e}"
        print(msg)
        return ret

    if not frame_name:
        return ThreadInfo(original_thread, original_frame)

    # --- Checkout to the target frame ---
    target_frame = None
    try:
        current_frame = gdb.newest_frame()
        if not current_frame:
            raise gdb.error("Current thread has no frames.")

        while current_frame:
            print(f"Frame: {current_frame.name()}")
            if current_frame.name() == frame_name:
                target_frame = current_frame
                break
            try:
                current_frame = current_frame.older()
            except gdb.error:
                current_frame = None
                break

        if target_frame:
            target_frame.select()
        else:
            msg = f"Frame '{frame_name}' not found in thread {target_thread.num}."
            print(msg)
            return ret
    except gdb.error as e:
        msg = f"Error finding/selecting frame: {e}"
        print(msg)
        return ret
    return ret


def _switch_to_aux_thread() -> Optional[ThreadInfo]:
    """Switch to the auxiliary thread and frame."""
    return _switch_to_thread(_THREAD_NAME, _FRAME_NAME)


def _check_proclet(proclet_id_str: str):
    """
    Checking proclet status whether if it is local.

    Args:
        proclet_id_str: The proclet ID as a string.

    Returns:
        A dictionary containing:
        - success (bool): Overall success of the operation leading to a check result.
        - message (str): A summary message.
        - proclet_info (dict): Information specific to the proclet checks.
                           Contains 'is_local', 'status',
    """
    result = {
        "success": False,
        "message": "Check did not complete.",
        "proclet_info": {
            "is_local": None,
            "status": None,
        },
    }
    proclet_info = result["proclet_info"]

    # --- Validate Input ---
    try:
        # Check if it looks like a number or hex address
        # parse_and_eval will handle the actual conversion later
        int(proclet_id_str, 0)
    except ValueError:
        msg = f"Invalid proclet_id '{proclet_id_str}'. Expected numerical value."
        result["message"] = msg
        result["success"] = False
        return result

    # --- Store Original State ---
    original_scheduler_lock = None
    original_thread: ThreadInfo = None
    hdr_val = None

    try:
        original_scheduler_lock = gdb.parameter("scheduler-locking")
        gdb.execute("set scheduler-locking on", to_string=True)
        # Checkout to the aux thread.
        original_thread = _switch_to_aux_thread()

        # --- Get the ProcletHeader pointer ---
        hdr_expr = f"({_PROCLET_TYPE}*) {proclet_id_str}"
        try:
            hdr_val = gdb.parse_and_eval(hdr_expr)
            # Set convenience variable $hdr for debugging/user inspection
            gdb.execute(f"set $proclet_hdr = {hdr_expr}", to_string=True)

            if str(hdr_val) == "0x0" or str(hdr_val).endswith(
                "<void>"
            ):  # Check for null or potentially bad cast
                result["message"] = (
                    f"Proclet header pointer {proclet_id_str} is NULL or invalid."
                )
                result["success"] = False
                return result
        except gdb.error as e:
            result["message"] = f"Error evaluating proclet header: {e}"
            result["success"] = False
            return result

        # --- Check is_local() ---
        try:
            is_local_raw = gdb.parse_and_eval("(bool) $proclet_hdr->is_local()")
            proclet_info["is_local"] = bool(is_local_raw)

            status_val = gdb.parse_and_eval("(uint8_t) $proclet_hdr->status()")
            proclet_info["status"] = int(status_val)

            if proclet_info["is_local"]:
                result["message"] = "Found a local proclet."
                result["success"] = True
            else:
                result["message"] = "Not found a local proclet."
                result["success"] = True
        except gdb.error as e:
            result["message"] = f"Error calling is_local(): {e}"
            result["success"] = False
            return result
    except Exception as e:
        err_msg = f"Unexpected Python error: {e}\n{traceback.format_exc()}"
        result["message"] = f"{err_msg}"
        result["success"] = False
    finally:
        # --- Restore Original State ---
        # Switch back to original thread and frame
        original_thread.switch()
        # Restore scheduler locking
        if original_scheduler_lock is not None:
            try:
                gdb.execute(
                    f"set scheduler-locking {original_scheduler_lock}", to_string=True
                )
            except gdb.error as e:
                print("Restore Scheduler Lock", "Failure", f"Error: {e}")
    return result


def _get_proclet_heap(proclet_id_str: str):
    result = _check_proclet(proclet_id_str)

    proclet_info = result["proclet_info"]
    proclet_info["copy_start"] = 0
    proclet_info["copy_end"] = 0
    proclet_info["copy_len"] = 0
    proclet_info["full_heap_size"] = 0
    proclet_info["heap_content"] = ""

    is_success = bool(result["success"])
    is_local = bool(result["proclet_info"]["is_local"])
    if not is_success:
        result["message"] = "Proclet check failed."
        return result
    if not is_local:
        # non-local proclet is considered as a failure in this context
        result["success"] = False
        result["message"] = "Proclet is not local."
        return result

    # --- Store Original State ---
    original_scheduler_lock = None
    original_thread: ThreadInfo = None
    hdr_val = None

    try:
        original_scheduler_lock = gdb.parameter("scheduler-locking")
        gdb.execute("set scheduler-locking on", to_string=True)
        original_thread = _switch_to_aux_thread()

        # --- Get the ProcletHeader pointer ---
        hdr_expr = f"({_PROCLET_TYPE}*) {proclet_id_str}"
        # skip validation considering the `check_proclet` has already been done.
        hdr_val = gdb.parse_and_eval(hdr_expr)
        gdb.execute(f"set $proclet_hdr = {hdr_expr}", to_string=True)
        copy_start = int(gdb.parse_and_eval("(uint64_t) $proclet_hdr->copy_start"))
        slab_base = int(gdb.parse_and_eval("(uint64_t) $proclet_hdr->slab.get_base()"))
        slab_usage = int(
            gdb.parse_and_eval("(uint64_t) $proclet_hdr->slab.get_usage()")
        )
        copy_len = slab_base - copy_start + slab_usage
        copy_end = copy_start + copy_len

        gdb.execute("set $hp_size = $proclet_hdr->heap_size()", to_string=True)
        heap_size_expr = (
            "(uint64_t) ((($hp_size - 1) / (nu::kPageSize + 1)) * nu::kPageSize)"
        )
        full_heap_size = int(gdb.parse_and_eval(heap_size_expr))

        proclet_info["copy_start"] = copy_start
        proclet_info["copy_end"] = copy_end
        proclet_info["copy_len"] = copy_len
        proclet_info["full_heap_size"] = full_heap_size

        inferior = gdb.selected_inferior()
        if not inferior:
            raise gdb.error("No inferior process selected.")
        if not inferior.is_valid():
            raise gdb.error("Inferior process is not valid.")

        mem_view = inferior.read_memory(copy_start, copy_len)
        hex_string = binascii.hexlify(mem_view).decode("ascii")
        proclet_info["heap_content"] = hex_string
    except Exception as e:
        err_msg = f"Unexpected Python error: {e}\n{traceback.format_exc()}"
        result["message"] = f"{err_msg}"
        result["success"] = False
    finally:
        # --- Restore Original State ---
        # Switch back to original thread and frame
        original_thread.switch()
        # Restore scheduler locking
        if original_scheduler_lock is not None:
            try:
                gdb.execute(
                    f"set scheduler-locking {original_scheduler_lock}", to_string=True
                )
            except gdb.error as e:
                print("Restore Scheduler Lock", "Failure", f"Error: {e}")
    return result


def _restore_proclet_heap(start_addr: int, data_len: int, data: str):
    result = {
        "success": False,
        "message": "",
    }

    # --- Store Original State ---
    original_scheduler_lock = None
    original_thread: ThreadInfo = None

    try:
        original_scheduler_lock = gdb.parameter("scheduler-locking")
        gdb.execute("set scheduler-locking on", to_string=True)
        original_thread = _switch_to_aux_thread()

        # --- Restore Memory ---
        inferior = gdb.selected_inferior()
        if not inferior:
            raise gdb.error("No inferior process selected.")
        if not inferior.is_valid():
            raise gdb.error("Inferior process is not valid.")

        data_bytes = binascii.unhexlify(data)
        inferior.write_memory(start_addr, data_bytes, data_len)

        result["success"] = True
        result["message"] = "Proclet heap is restored successfully."
    except Exception as e:
        err_msg = f"Unexpected Python error: {e}\n{traceback.format_exc()}"
        result["message"] = f"{err_msg}"
    finally:
        # --- Restore Original State ---
        # Switch back to original thread and frame
        original_thread.switch()
        # Restore scheduler locking
        if original_scheduler_lock is not None:
            try:
                gdb.execute(
                    f"set scheduler-locking {original_scheduler_lock}", to_string=True
                )
            except gdb.error as e:
                print("Restore Scheduler Lock", "Failure", f"Error: {e}")
    return result


def _cleanup_proclet_heap(proclet_id_str: str, full_heap_size: int):
    result = {
        "success": False,
        "message": "",
    }

    # --- Store Original State ---
    original_scheduler_lock = None
    original_thread: ThreadInfo = None

    try:
        original_scheduler_lock = gdb.parameter("scheduler-locking")
        gdb.execute("set scheduler-locking on", to_string=True)
        original_thread = _switch_to_aux_thread()

        gdb.execute(f"set $base = (void*) {proclet_id_str}", to_string=True)
        base = int(gdb.parse_and_eval("(uint64_t) $base"))
        gdb.execute(f"set $hp_size = (size_t) {full_heap_size}", to_string=True)
        ret_val = gdb.parse_and_eval("mmap((void*) $base, (size_t) $hp_size, (int) 3, (int) 0x4032, (int) -1, (long) 0)")
        ret_val = int(ret_val.cast(gdb.lookup_type("uint64_t")))
        safe = (ret_val == base)
        if safe:
            result["success"] = True
            result["message"] = "Proclet heap is cleaned up successfully."
        else:
            result["success"] = False
            expected = int(gdb.parse_and_eval("(uint64_t) $base"))
            real = int(gdb.parse_and_eval("(uint64_t) $cleanup_ret"))
            result["message"] = (
                f"mmap returned a different address than expected. expected: {expected}, real: {real}"
            )
    except Exception as e:
        err_msg = f"Unexpected Python error: {e}\n{traceback.format_exc()}"
        result["message"] = f"{err_msg}"
    finally:
        # --- Restore Original State ---
        # Switch back to original thread and frame
        original_thread.switch()
        # Restore scheduler locking
        if original_scheduler_lock is not None:
            try:
                gdb.execute(
                    f"set scheduler-locking {original_scheduler_lock}", to_string=True
                )
            except gdb.error as e:
                print("Restore Scheduler Lock", "Failure", f"Error: {e}")
    return result


# --------------------------------------------------------------------
# GDB MI Command (-check-proclet)
# --------------------------------------------------------------------
class CheckProcletMiCommand(gdb.MICommand):
    """GDB MI command to check proclet status."""

    def __init__(self, name):
        self.cmd_name = name
        super(CheckProcletMiCommand, self).__init__(name)

    def invoke(self, argv):
        if len(argv) != 1:
            return {
                "success": str(False).lower(),
                "message": f"Usage: {self.cmd_name} <proclet-id>",
            }

        proclet_id_str = argv[0]
        check_result = _check_proclet(proclet_id_str)
        mi_result = {
            "success": str(check_result["success"]).lower(),
            "message": check_result["message"],
            "is_local": str(check_result["proclet_info"]["is_local"]).lower()
            if check_result["proclet_info"]["is_local"] is not None
            else "none",
            "status": str(check_result["proclet_info"]["status"])
            if check_result["proclet_info"]["status"] is not None
            else "none",
        }
        return mi_result


class GetProcletHeapMiCommand(gdb.MICommand):
    def __init__(self, name):
        self.cmd_name = name
        super(GetProcletHeapMiCommand, self).__init__(name)

    def invoke(self, argv):
        if len(argv) != 1:
            return {
                "success": str(False).lower(),
                "message": f"Usage: {self.cmd_name} <proclet-id>",
            }

        proclet_id_str = argv[0]
        result = _get_proclet_heap(proclet_id_str)
        return {
            "success": str(result["success"]).lower(),
            "message": result["message"],
            "start": str(result["proclet_info"]["copy_start"]),
            "end": str(result["proclet_info"]["copy_end"]),
            "len": str(result["proclet_info"]["copy_len"]),
            "full_heap_size": str(result["proclet_info"]["full_heap_size"]),
            "heap_content": str(result["proclet_info"]["heap_content"]),
        }


class RestoreProcletHeapMiCommand(gdb.MICommand):
    def __init__(self, name):
        self.cmd_name = name
        super(RestoreProcletHeapMiCommand, self).__init__(name)

    def invoke(self, argv):
        if len(argv) != 3:
            return {
                "success": str(False).lower(),
                "message": f"Usage: {self.cmd_name} <start-addr> <data-len> <data>",
            }

        start_addr = int(argv[0])
        data_len = int(argv[1])
        data = str(argv[2])
        result = _restore_proclet_heap(start_addr, data_len, data)
        return {
            "success": str(result["success"]).lower(),
            "message": result["message"],
        }


class CleanProcletHeapMiCommand(gdb.MICommand):
    def __init__(self, name):
        self.cmd_name = name
        super(CleanProcletHeapMiCommand, self).__init__(name)

    def invoke(self, argv):
        if len(argv) != 2:
            return {
                "success": False,
                "message": f"Usage: {self.cmd_name} <proclet-id> <full-heap-size>",
            }

        proclet_id_str = str(argv[0])
        full_heap_size = int(argv[1])
        result = _cleanup_proclet_heap(proclet_id_str, full_heap_size)
        return {
            "success": str(result["success"]).lower(),
            "message": result["message"],
        }


# --------------------------------------------------------------------
# Regular GDB Command (check-proclet)
# --------------------------------------------------------------------
class CheckProcletCommand(gdb.Command):
    """Checks the status of a nu::ProcletHeader based on its ID. (User Command)

    Usage: check-proclet <proclet_id>

    Provides human-readable output of the proclet check process.
    """

    def __init__(self, name: str):
        self.cmd_name = name
        super(CheckProcletCommand, self).__init__(name, gdb.COMMAND_USER)

    def invoke(self, arg, from_tty):
        args = gdb.string_to_argv(arg)
        if len(args) != 1:
            print(f"Usage: {self.cmd_name} <proclet_id>")
            return

        proclet_id_str = args[0]
        print(f"--- Checking Proclet ID: {proclet_id_str} ---")

        # Call the core logic function
        check_result = _check_proclet(proclet_id_str)

        success = bool(check_result["success"])
        if success:
            print("Proclet check completed successfully.")
            pi = check_result["proclet_info"]
            if pi["is_local"] is not None:
                print(f"  Proclet Is Local: {pi['is_local']}")
            if pi["status"] is not None:
                print(f"  Proclet Status: {pi['status']}")
        else:
            print("Proclet check failed.")
            print(f"  Error Message: {check_result['message']}")


class GetProcletHeapCommand(gdb.Command):
    def __init__(self, name: str):
        self.cmd_name = name
        super(GetProcletHeapCommand, self).__init__(name, gdb.COMMAND_USER)

    def invoke(self, arg, from_tty):
        args = gdb.string_to_argv(arg)
        if len(args) != 1:
            print(f"Usage: {self.cmd_name} <proclet_id>")
            return

        proclet_id_str = args[0]
        print(f"--- Get Heap information for Proclet ID: {proclet_id_str} ---")

        result = _get_proclet_heap(proclet_id_str)
        success = bool(result["success"])
        if success:
            print("Proclet heap information retrieved successfully.")
            pi = result["proclet_info"]
            if pi["copy_start"] is not None:
                print(f"  Copy Start: {pi['copy_start']}")
            if pi["copy_end"] is not None:
                print(f"  Copy End: {pi['copy_end']}")
            if pi["copy_len"] is not None:
                print(f"  Copy Length: {pi['copy_len']}")
            if pi["full_heap_size"] is not None:
                print(f"  Full Heap Size: {pi['full_heap_size']}")
            if pi["heap_content"] is not None:
                print(f"  Heap Content: {pi['heap_content']}")
        else:
            print("Proclet heap information retrieval failed.")
            print(f"  Error Message: {result['message']}")
            return


class RestoreProcletHeapCommand(gdb.Command):
    def __init__(self, name: str):
        self.cmd_name = name
        super(RestoreProcletHeapCommand, self).__init__(name, gdb.COMMAND_USER)

    def invoke(self, arg, from_tty):
        args = gdb.string_to_argv(arg)
        if len(args) != 3:
            print(f"Usage: {self.cmd_name} <start-addr> <data-len> <data>")
            return

        start_addr = int(args[0])
        data_len = int(args[1])
        data = str(args[2])

        result = _restore_proclet_heap(start_addr, data_len, data)
        success = bool(result["success"])
        if success:
            print("Proclet heap restored successfully.")
        else:
            print("Proclet heap restoration failed.")
            print(f"  Error Message: {result['message']}")
            return


class CleanProcletHeapCommand(gdb.Command):
    def __init__(self, name: str):
        self.cmd_name = name
        super(CleanProcletHeapCommand, self).__init__(name, gdb.COMMAND_USER)

    def invoke(self, arg, from_tty):
        args = gdb.string_to_argv(arg)
        if len(args) != 2:
            print(f"Usage: {self.cmd_name} <proclet-id> <full-heap-size>")
            return

        proclet_id = str(args[0])
        full_heap_size = int(args[1])

        result = _cleanup_proclet_heap(proclet_id, full_heap_size)
        success = bool(result["success"])
        if success:
            print("Proclet heap cleanup successfully.")
        else:
            print("Proclet heap cleanup failed.")
            print(f"  Error Message: {result['message']}")
            return


CheckProcletMiCommand("-check-proclet")
CheckProcletCommand("check-proclet")

GetProcletHeapMiCommand("-get-proclet-heap")
GetProcletHeapCommand("get-proclet-heap")

RestoreProcletHeapMiCommand("-restore-proclet-heap")
RestoreProcletHeapCommand("restore-proclet-heap")

CleanProcletHeapMiCommand("-clean-proclet-heap")
CleanProcletHeapCommand("clean-proclet-heap")

print(
    "Proclet commands ('check-proclet', '-check-proclet', 'get-proclet-heap', '-get-proclet-heap', 'restore-proclet-heap', '-restore-proclet-heap', 'clean-proclet-heap', '-clean-proclet-heap') loaded."
)
