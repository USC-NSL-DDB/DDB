#!/usr/bin/env python3

import os
import re
import signal
import subprocess
import sys
import argparse

from typing import List, Union

from ddb.data_struct import TargetFramework
from ddb.gdb_manager import GdbManager
from ddb.logging import logger
from ddb.gdb_session import GdbMode, GdbSessionConfig, StartMode
from ddb.utils import *
from ddb.config import GlobalConfig
# import debugpy

# try:
#     debugpy.listen(("localhost", 5678))
#     print("Waiting for debugger attach")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")

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

def bootFromNuConfig(gdb_manager: GdbManager):
    gdb_manager.start()
    while True:
        cmd = input("(gdb) ").strip()
        cmd = f"{cmd}\n"
        gdb_manager.write(cmd)

def bootServiceWeaverKube(gdb_manager: GdbManager):
    gdb_manager.start()
    while True:
        cmd = input(f"({gdb_manager.state_mgr.get_current_gthread()})(gdb) ").strip()
        cmd = f"{cmd}\n"
        if cmd is not None:
            gdb_manager.write(cmd)

terminated = False
gdb_manager:GdbManager = None

def handle_interrupt(signal_num, frame):
    global terminated, gdb_manager
    dev_print(f"Received interrupt")
    if not terminated:
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

def main():
    global gdb_manager, terminated
    gdb_manager=GdbManager()
    signal.signal(signal.SIGINT, handle_interrupt)
    parser = argparse.ArgumentParser(
        description="interactive debugging for distributed software.",
    )

    parser.add_argument(
        "config",
        metavar="conf_file",
        type=str,
        help="Path of the debugging config file."
    )

    args = parser.parse_args()

    if (args.config is not None) and GlobalConfig.load_config(str(args.config)):
        logger.info(f"Loaded config. content: \n{GlobalConfig.get()}")    
    else:
        logger.info(f"Configuration file is not provided or something goes wrong. Skipping...")    

    # TODO: implement the following functions
    # exec_pretasks(config_data)

    global_config = GlobalConfig.get()
    try:
        if global_config.framework == TargetFramework.SERVICE_WEAVER_K8S:
            bootServiceWeaverKube(gdb_manager)
        elif global_config.framework == TargetFramework.NU:
            bootFromNuConfig(gdb_manager)
        else:
            bootFromNuConfig(gdb_manager)
    except KeyboardInterrupt:
        logger.info("Received interrupt signal. Exiting...")

        if gdb_manager:
            gdb_manager.cleanup()

        # TODO: implement the following functions
        # if config_data is not None:
        #     exec_posttasks(config_data)

        try:
            sys.exit(130)
        except SystemExit:
            os._exit(130)
    
    pass 

if __name__ == "__main__":
    main()
