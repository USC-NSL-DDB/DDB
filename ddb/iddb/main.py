import os
import signal
import sys
import argparse
import asyncio

from typing import List, Optional, Union

from iddb.event_loop import AsyncSSHLoop, GlobalRunningLoop
from iddb.gdb_session import SessionCreationTaskQueue
from iddb.global_handler import GlobalHandler
from iddb.mi_formatter import MIFormatter
from iddb.response_processor import ResponseProcessor
from iddb.data_struct import TargetFramework
from iddb.gdb_manager import GdbManager
from iddb.logging import logger
from iddb.utils import *
from iddb.config import GlobalConfig
from iddb.about import ENABLE_DEBUGGER
from iddb.helper.tracer import VizTracerHelper
from iddb import globals

if ENABLE_DEBUGGER:
    try:
        import debugpy
        debugpy.listen(("localhost", 5678))
        print("Waiting for debugger attach")
        debugpy.wait_for_client()
    except Exception as e:
        print(f"Failed to attach debugger: {e}")

async def exec_cmd(cmd: Union[List[str], str]):
    if isinstance(cmd, str):
        cmd = [cmd]
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await proc.communicate()
    eprint(stdout.decode("utf-8"))
    eprint(stderr.decode("utf-8"))

async def exec_task(task: dict):
    name = task.get("name", "Unnamed")
    command = task.get("command")

    if not command:
        eprint("Didn't specify command.")
        return

    eprint(f"Executing task: {name}, command: {command}")
    await exec_cmd(command)

async def exec_pretasks(config_data):
    if ("PreTasks" in config_data) and config_data["PreTasks"]:
        for task in config_data["PreTasks"]:
            await exec_task(task)

async def exec_posttasks(config_data):
    if ("PostTasks" in config_data) and config_data["PostTasks"]:
        for task in config_data["PostTasks"]:
            await exec_task(task)

async def run_cmd_loop():
    while True:
        try:
            cmd = await asyncio.get_event_loop().run_in_executor(None, input, "(gdb) ")
            cmd = f"{cmd.strip()}\n"
            await globals.DBG_MANAGER.write_async(cmd)
            raw_cmd = cmd.strip()
            if raw_cmd == "exit" or raw_cmd == "-gdb-exit":
                break
        except EOFError:
            print("\nNo input received")
        except asyncio.CancelledError as e:
            logger.info(f"run_cmd_loop cancelled: {e}")
            break
    proper_cleanup()

async def ddb_exit():
    with globals.G_LOCK:
        if not globals.TERMINATED:
            logger.debug("Exiting ddb...")
            print("[ TOOL MI OUTPUT ]")
            print(MIFormatter.format("*", "stopped", {"reason": "exited"}, None))

            # VizTracerHelper.deinit()

            globals.TERMINATED = True
            if globals.DBG_MANAGER:
                await globals.DBG_MANAGER.cleanup_async()
            
            GlobalRunningLoop().stop()
            AsyncSSHLoop().stop()

            try:
                sys.exit(0)
            except SystemExit:
                os._exit(0)

async def bootFromNuConfig(gdb_manager: GdbManager):
    await gdb_manager.start_async()
    await run_cmd_loop()

async def bootServiceWeaverKube(gdb_manager: GdbManager):
    await gdb_manager.start_async()
    await run_cmd_loop()

def proper_cleanup(signal_name: Optional[str] = None):
    if globals.TERMINATED: return
    if signal_name:
        logger.info(f"Caught signal [{signal_name}]...")

    # Ensure the clean up logic is happening in the main loop.
    if globals.MAIN_LOOP.is_running():
        try:
            if asyncio.get_running_loop() == globals.MAIN_LOOP:
                asyncio.create_task(ddb_exit())
            else:
                asyncio.run_coroutine_threadsafe(ddb_exit(), globals.MAIN_LOOP)
        except Exception as e:
            print(f"Error in proper_cleanup: {e}")

def prepare_args() -> argparse.Namespace:
    # pre-parser to handle --debug and --version flags
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--debug", action="store_true", help="Enable debug mode.")
    parser.add_argument(
        "-v", "--version", action="store_true", help="Version of ddb."
    )
    args, remaining_argv = parser.parse_known_args()

    if args.debug:
        try:
            import debugpy
            debugpy.listen(("localhost", 5678))
            print("Waiting for debugger attach")
            debugpy.wait_for_client()
        except ImportError as ie:
            print(f"Failed to import debugpy: {ie}")
            sys.exit(1)
        except Exception as e:
            print(f"Failed to attach debugger: {e}")
            sys.exit(1)

    if args.version:
        from iddb import about
        print(about.__version__)
        sys.exit(0)

    parser = argparse.ArgumentParser(description="Interactive debugging for distributed software.")
    parser.add_argument("config", metavar="conf_file", type=str, help="Path of the debugging config file.")
    args = parser.parse_args(remaining_argv)
    return args

def eager_init():
    GlobalRunningLoop()
    AsyncSSHLoop()
    _ = ResponseProcessor.inst()

    tq = SessionCreationTaskQueue.inst()
    tq.start_workers()

def main():
    # VizTracerHelper.init()
    eager_init()

    async def run_async():
        args = prepare_args()

        loop = asyncio.get_event_loop()
        globals.MAIN_LOOP = loop

        loop.add_signal_handler(signal.SIGINT, proper_cleanup, "SIGINT")
        loop.add_signal_handler(signal.SIGTERM, proper_cleanup, "SIGTERM")

        GlobalHandler.DDB_EXIT_HANDLE = lambda: proper_cleanup()

        globals.DBG_MANAGER = GdbManager()

        if (args.config is not None) and GlobalConfig.load_config(str(args.config)):
            logger.info(f"Loaded config. content: \n{GlobalConfig.get()}")    
        else:
            logger.info(f"Configuration file is not provided or something goes wrong. Skipping...")    

        global_config = GlobalConfig.get()
        try:
            if global_config.framework == TargetFramework.SERVICE_WEAVER_K8S:
                from kubernetes import config
                try:
                    await bootServiceWeaverKube(globals.DBG_MANAGER)
                except Exception as e:
                    print("fail to load kubernetes config, check path again")
            elif global_config.framework == TargetFramework.NU:
                await bootFromNuConfig(globals.DBG_MANAGER)
            else:
                await bootFromNuConfig(globals.DBG_MANAGER)
        except KeyboardInterrupt:
            logger.debug("Received keyboard signal.")
            proper_cleanup()

    try:
        asyncio.run(run_async(), debug=False)
    except Exception as e:
        logger.error(f"Failed to run main function [run_async]: {e}")
        
if __name__ == "__main__":
    main()
