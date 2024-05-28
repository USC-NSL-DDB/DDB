#!/usr/bin/env python3

import os
import re
import signal
import subprocess
from typing import List, Union
from pprint import pprint
from ddb.gdb_manager import GdbManager
from ddb.logging import logger
from yaml import safe_load, YAMLError
from ddb.gdb_session import GdbMode, GdbSessionConfig, StartMode
from ddb.gdbserver_starter import SSHRemoteServerCred, SSHRemoteServerClient
from ddb.utils import *
import sys
import argparse
# import debugpy

# try:
#     debugpy.listen(("localhost", 5678))
#     print("Waiting for debugger attach")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")
gdb_manager:GdbManager=None

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

def bootFromNuConfig(config_data=None):
    gdbSessionConfigs: List[GdbSessionConfig] = []
    prerun_cmds = None
    global gdb_manager
    if config_data:
        components = config_data["Components"] if "Components" in config_data else []
        prerun_cmds = config_data["PrerunGdbCommands"] if "PrerunGdbCommands" in config_data else None

        for component in components:
            sessionConfig = GdbSessionConfig()

            sessionConfig.tag = component.get("tag", None)
            sessionConfig.start_mode = component.get("startMode", StartMode.BINARY)
            sessionConfig.attach_pid = component.get("pid", 0)
            sessionConfig.binary = component.get("bin", None)
            sessionConfig.cwd = component.get("cwd", os.getcwd())
            sessionConfig.args = component.get("args", [])
            sessionConfig.run_delay = component.get("run_delay", 0)
            sessionConfig.sudo = component.get("sudo", False)

            sessionConfig.gdb_mode = GdbMode.REMOTE if \
                "mode" in component.keys() and component["mode"] == "remote" \
                else GdbMode.LOCAL
            if sessionConfig.gdb_mode == GdbMode.REMOTE:
                sessionConfig.remote_port = component["remote_port"]
                sessionConfig.remote_host = component["cred"]["hostname"]
                sessionConfig.username = component["cred"]["user"]
                remote_cred = SSHRemoteServerCred(
                    port=sessionConfig.remote_port,
                    bin=os.path.join(sessionConfig.cwd, sessionConfig.binary), # respect current working directoy.
                    hostname=sessionConfig.remote_host,
                    username=sessionConfig.username
                )
                sessionConfig.remote_gdbserver = SSHRemoteServerClient(
                    cred=remote_cred)

            gdbSessionConfigs.append(sessionConfig)
    
    gdb_manager = GdbManager(gdbSessionConfigs, prerun_cmds)

    while True:
        cmd = input("(gdb) ").strip()
        cmd = f"{cmd}\n"
        gdb_manager.write(cmd)

def bootServiceWeaverKube(config_data=None):
    from kubernetes import config as kubeconfig, client as kubeclient
    from ddb.gdbserver_starter import KubeRemoteSeverClient
    global gdb_manager
    kubeconfig.load_incluster_config()
    clientset = kubeclient.CoreV1Api()
    prerun_cmds = config_data.get("PrerunGdbCommands",[])
    config_metadata=config_data.get("Components",{})
    kube_namespace = config_metadata.get("kube_namespace","default")
    sw_name = config_metadata.get("binary_name","serviceweaver")
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
            # sessionConfig.gdb_config_cmds = ["source ./noobextension.py"]
            gdbSessionConfigs.append(sessionConfig)
        else:
            eprint(i.status.pod_ip, i.metadata.name,
                   "cannot locate service weaver process:", sw_name)

    gdb_manager = GdbManager(gdbSessionConfigs, prerun_cmds)
    
    while True:
        cmd = input(f"({gdb_manager.state_mgr.get_current_gthread()})(gdb) ").strip()
        cmd = f"{cmd}\n"
        if cmd is not None:
            gdb_manager.write(cmd)
terminated = False
def handle_interrupt(signal_num, frame):
    global terminated
    dev_print(f"Received interrupt")
    if not terminated:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        terminated=True
        if gdb_manager:
            gdb_manager.cleanup()
        if config_data is not None:
            exec_posttasks(config_data)

        try:
            sys.exit(130)
        except SystemExit:
            os._exit(130)

def main():
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

    config_data = None
    if args.config is not None:
        with open(str(args.config), "r") as fs:
            try:
                config_data = safe_load(fs)
                eprint("Loaded dbg config file:")
                pprint(config_data)
            except YAMLError as e:
                eprint(f"Failed to read the debugging config. Error: {e}")

        if not config_data:
            eprint("Debugging config is required!")
            exit(1)

        exec_pretasks(config_data)

    gdb_manager: GdbManager = None
    terminated=False
    try:
        if config_data["Framework"] == "serviceweaver_kube":
            bootServiceWeaverKube(config_data)
        elif config_data["Framework"] == "Nu":
            bootFromNuConfig(config_data)
        else:
            bootFromNuConfig(config_data)
    except KeyboardInterrupt:
        pass

if __name__ == "__main__":
    main()
