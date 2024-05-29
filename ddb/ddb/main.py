#!/usr/bin/env python3

import os
import re
import subprocess
import sys
import argparse

from typing import List, Union

from ddb.gdb_manager import GdbManager
from ddb.logging import logger
from ddb.gdb_session import GdbMode, GdbSessionConfig, StartMode
from ddb.utils import *
from ddb.config import GlobalConfig

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

def bootFromNuConfig(gdb_manager: GdbManager=None):
    gdb_manager = GdbManager()

    while True:
        cmd = input("(gdb) ").strip()
        cmd = f"{cmd}\n"
        gdb_manager.write(cmd)

def bootServiceWeaverKube():
    from kubernetes import config as kubeconfig, client as kubeclient
    from gdbserver_starter import KubeRemoteSeverClient

    kubeconfig.load_incluster_config()
    clientset = kubeclient.CoreV1Api()
    global gdb_manager, config_data
    #prerun_cmds = config_data["PrerunGdbCommands"] if "PrerunGdbCommands" in config_data else None

    kube_namespace = "default"
    sw_name = "serviceweaver1"
    selector_label = f"serviceweaver/app={sw_name}"
    pods = clientset.list_namespaced_pod(
        namespace=kube_namespace, label_selector=selector_label)
    gdbSessionConfigs: List[GdbSessionConfig] = []
    for i in pods.items:
        dev_print("%s\t%s\t%s" %
              (i.status.pod_ip, i.metadata.namespace, i.metadata.name))
        remoteServerConn = KubeRemoteSeverClient(
            i.metadata.name, i.metadata.namespace)
        remoteServerConn.connect()
        output = remoteServerConn.execute_command(['ps', '-eo', "pid,comm"])
        # Use a regular expression to find the PID for 'serviceweaver1'
        match = re.search(r'(\d+)\s+{}'.format(sw_name), output)
        if match:
            pid = match.group(1)
            # sessionConfig = GdbSessionConfig()
            # sessionConfig.remote_port = 30001
            # sessionConfig.remote_host = i.status.pod_ip
            # print("remote host type:", type(i.status.pod_ip))
            # sessionConfig.gdb_mode = GdbMode.REMOTE
            # sessionConfig.remote_gdbserver = remoteServerConn
            # sessionConfig.tag = i.status.pod_ip
            # sessionConfig.start_mode = StartMode.ATTACH
            # sessionConfig.attach_pid = int(pid)
            # sessionConfig.gdb_config_cmds = ["source ./noobextension.py"]
            sessionConfig= GdbSessionConfig()
            sessionConfig.remote_port=30001
            sessionConfig.remote_host=i.status.pod_ip
            dev_print("remote host type:", type(i.status.pod_ip))
            sessionConfig.gdb_mode=GdbMode.REMOTE
            sessionConfig.remote_gdbserver=remoteServerConn
            sessionConfig.tag=i.status.pod_ip
            sessionConfig.start_mode=StartMode.ATTACH
            sessionConfig.attach_pid=int(pid)
            sessionConfig.gdb_config_cmds=["source /usr/src/app/gdb_ext/noobextension.py"]
            # sessionConfig.gdb_config_cmds = ["source ./noobextension.py"]
            gdbSessionConfigs.append(sessionConfig)
        else:
            eprint(i.status.pod_ip, i.metadata.name,
                   "cannot locate service weaver process:", sw_name)

    gdb_manager = GdbManager(gdbSessionConfigs, [{"name":"load serviceweaver ext","command":"source /usr/src/app/gdb_ext/noobextension.py"},{"name": "enable async mode",
  "command": "set target-async on"}])
    
    while True:
        cmd = input(f"({gdb_manager.state_mgr.get_current_gthread()})(gdb) ").strip()
        cmd = f"{cmd}\n"
        if cmd is not None:
            gdb_manager.write(cmd)

def main():
    parser = argparse.ArgumentParser(
        description="interactive debugging for distributed software.",
    )

    parser.add_argument(
        "config",
        metavar="conf_file",
        nargs='?',
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

    gdb_manager: GdbManager = None
    try:
        bootFromNuConfig(gdb_manager)
        # bootServiceWeaverKube()
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
