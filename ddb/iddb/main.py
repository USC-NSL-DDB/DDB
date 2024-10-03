import os
import signal
import subprocess
import sys
import argparse

from typing import List, Union

from iddb.event_loop import GlobalRunningLoop
from iddb.response_processor import ResponseProcessor
from iddb.data_struct import TargetFramework
from iddb.gdb_manager import GdbManager
from iddb.logging import logger
from iddb.startup import cleanup_mosquitto_broker
from iddb.utils import *
from iddb.config import GlobalConfig

def exec_cmd(cmd: Union[List[str], str]):
    if isinstance(cmd, str):
        cmd = [cmd]
    result = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE
    )
    eprint(result.stdout.decode("utf-8"))
    eprint(result.stderr.decode("utf-8"))


def exec_task(task: dict):
    name = None
    command = None
    if "name" in task:
        name = task["name"]
    if "command" in task:
        command = task["command"]

    if not name:
        name = "Unnamed"
    if not command:
        eprint("Didn't specify command.")
        return

    eprint(f"Executing task: {name}, command: {command}")
    exec_cmd(command)

def exec_pretasks(config_data):
    if ("PreTasks" in config_data) and config_data["PreTasks"]:
        tasks = config_data["PreTasks"]
        for task in tasks:
            exec_task(task)

def exec_posttasks(config_data):
    if ("PostTasks" in config_data) and config_data["PostTasks"]:
        tasks = config_data["PostTasks"]
        for task in tasks:
            exec_task(task)

terminated = False
gdb_manager: GdbManager = None

def run_cmd_loop():
    while True:
        try:
            cmd = input("(gdb) ").strip()
            cmd = f"{cmd}\n"
            gdb_manager.write(cmd) 
            raw_cmd = cmd.strip()
            if raw_cmd == "exit" or raw_cmd == "-gdb-exit":
                break
        except EOFError:
            print("\nNo input received")
    ddb_exit()

def ddb_exit():
    global gdb_manager, terminated
    cleanup_mosquitto_broker()
    if not terminated:
        logger.info("Exiting ddb...")
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        terminated=True
        if gdb_manager:
            gdb_manager.cleanup()

        # TODO: reimplement the following functions
        # if config_data is not None:
        #     exec_posttasks(config_data)

        try:
            sys.exit(130)
        except SystemExit:
            os._exit(130)

def bootFromNuConfig(gdb_manager: GdbManager):
    gdb_manager.start()
    run_cmd_loop()

def bootServiceWeaverKube(gdb_manager: GdbManager):
    gdb_manager.start()
    run_cmd_loop()

def handle_interrupt(signal_num, frame):
    logger.debug("Handling interrupt...")
    ddb_exit()

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
    _ = ResponseProcessor.inst()
    GlobalRunningLoop()

def main():
    args = prepare_args()

    global gdb_manager, terminated
    signal.signal(signal.SIGINT, handle_interrupt)
    eager_init()
    gdb_manager = GdbManager()

    if (args.config is not None) and GlobalConfig.load_config(str(args.config)):
        logger.info(f"Loaded config. content: \n{GlobalConfig.get()}")    
    else:
        logger.info(f"Configuration file is not provided or something goes wrong. Skipping...")    

    # TODO: implement the following functions
    # exec_pretasks(config_data)

    global_config = GlobalConfig.get()
    try:
        if global_config.framework == TargetFramework.SERVICE_WEAVER_K8S:
            from kubernetes import config
            try:
                bootServiceWeaverKube(gdb_manager)
            except Exception as e:
                print("fail to laod kubernetes config, check path again")
        elif global_config.framework == TargetFramework.NU:
            bootFromNuConfig(gdb_manager)
        else:
            bootFromNuConfig(gdb_manager)
    except KeyboardInterrupt:
        logger.debug("Received interrupt signal.")
        ddb_exit()
    pass 

if __name__ == "__main__":
    main()
