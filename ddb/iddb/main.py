import os
import signal
import subprocess
import sys
import argparse
import asyncio

from typing import List, Union

from iddb.event_loop import AsyncSSHConnLoop, AsyncSSHLoop, GlobalRunningLoop
from iddb.global_handler import GlobalHandler
from iddb.mi_formatter import MIFormatter
from iddb.response_processor import ResponseProcessor
from iddb.data_struct import TargetFramework
from iddb.gdb_manager import GdbManager
from iddb.logging import logger
from iddb.startup import cleanup_mosquitto_broker
from iddb.utils import *
from iddb.config import GlobalConfig
from iddb.about import ENABLE_DEBUGGER
from iddb.helper.tracer import VizTracerHelper
from viztracer import VizTracer

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

terminated = False
gdb_manager: GdbManager = None

async def run_cmd_loop():
    while True:
        try:
            cmd = await asyncio.get_event_loop().run_in_executor(None, input, "(gdb) ")
            cmd = f"{cmd.strip()}\n"
            await gdb_manager.write_async(cmd)
            raw_cmd = cmd.strip()
            if raw_cmd == "exit" or raw_cmd == "-gdb-exit":
                break
        except EOFError:
            print("\nNo input received")
        except asyncio.CancelledError as e:
            print(f"Error: {e}")
            break
    await ddb_exit()

async def stop_event_loop(loop: asyncio.AbstractEventLoop):
    """Gracefully stop and clean up the given event loop."""
    # Cancel all running tasks
    tasks = [t for t in asyncio.all_tasks(loop) if not t.done()]
    for task in tasks:
        task.cancel()
    print(f"Cancelling {len(tasks)} tasks...")

    # Wait for all tasks to finish
    try:
        await asyncio.gather(*tasks, return_exceptions=True)
    except asyncio.CancelledError:
        pass
    print("All tasks cancelled.")

    # Stop the loop
    loop.stop()
    print("Event loop stopped.")

    # Close the loop
    loop.close()
    print("Event loop closed.")

async def ddb_exit():
    global gdb_manager, terminated
    cleanup_mosquitto_broker()
    if not terminated:
        logger.debug("Exiting ddb...")
        print("[ TOOL MI OUTPUT ]")
        print(MIFormatter.format("*", "stopped", {"reason": "exited"}, None))

        VizTracerHelper.deinit()

        terminated = True
        if gdb_manager:
            await gdb_manager.cleanup_async()  # Assuming GdbManager has async cleanup

        # GlobalRunningLoop().get_loop().close()
        # AsyncSSHLoop().get_loop().close()
        # AsyncSSHConnLoop().get_loop().close()
        # asyncio.get_event_loop().close()

        # GlobalRunningLoop().stop()
        # AsyncSSHLoop().stop()
        # AsyncSSHConnLoop().stop()
        # asyncio.get_event_loop().stop()
        # GlobalRunningLoop().stop()
        # await stop_event_loop(GlobalRunningLoop().get_loop())
        # await stop_event_loop(AsyncSSHLoop().get_loop())
        # await stop_event_loop(AsyncSSHConnLoop().get_loop())
        # await stop_event_loop(asyncio.get_event_loop())

        try:
            sys.exit(130)
        except SystemExit:
            os._exit(130)

async def bootFromNuConfig(gdb_manager: GdbManager):
    await gdb_manager.start_async()  # Assuming GdbManager has async start
    await run_cmd_loop()

async def bootServiceWeaverKube(gdb_manager: GdbManager):
    await gdb_manager.start_async()  # Assuming GdbManager has async start
    await run_cmd_loop()

MAIN_LOOP: asyncio.AbstractEventLoop = None

def handle_interrupt():
    logger.debug("Handling interrupt...")
    asyncio.create_task(ddb_exit())

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
    AsyncSSHConnLoop()
    _ = ResponseProcessor.inst()

def main():
    VizTracerHelper.init()
    eager_init()

    async def run_async():
        global gdb_manager
        args = prepare_args()

        loop = asyncio.get_event_loop()

        loop.add_signal_handler(signal.SIGINT, handle_interrupt)
        # signal.signal(signal.SIGINT, handle_interrupt)

        GlobalHandler.DDB_EXIT_HANDLE = lambda: asyncio.create_task(ddb_exit())

        gdb_manager = GdbManager()

        if (args.config is not None) and GlobalConfig.load_config(str(args.config)):
            logger.info(f"Loaded config. content: \n{GlobalConfig.get()}")    
        else:
            logger.info(f"Configuration file is not provided or something goes wrong. Skipping...")    

        global_config = GlobalConfig.get()
        try:
            if global_config.framework == TargetFramework.SERVICE_WEAVER_K8S:
                from kubernetes import config
                try:
                    await bootServiceWeaverKube(gdb_manager)
                except Exception as e:
                    print("fail to load kubernetes config, check path again")
            elif global_config.framework == TargetFramework.NU:
                await bootFromNuConfig(gdb_manager)
            else:
                await bootFromNuConfig(gdb_manager)
        except KeyboardInterrupt:
            logger.debug("Received interrupt signal.")
            await ddb_exit()

    asyncio.run(run_async())

if __name__ == "__main__":
    # asyncio.run(main())
    main()
