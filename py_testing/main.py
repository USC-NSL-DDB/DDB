import os
import re
import subprocess
from typing import List, Union
from pprint import pprint
from gdb_manager import GdbManager
from yaml import safe_load, YAMLError
from gdb_session import GdbMode, GdbSessionConfig, StartMode
from gdbserver_starter import KubeRemoteSeverClient, SSHRemoteServerCred, SSHRemoteSeverClient
from utils import *
from kubernetes import config as kubeconfig, client as kubeclient
import sys
import argparse

# ARGS = [
#     ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.3"],
#     ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.4"],
#     ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.5", "-m"],
# ]


def main():
    global gdb_manager, config_data

    components = config_data["Components"]
    gdbSessionConfigs: List[GdbSessionConfig] = []
    for component in components:
        sessionConfig = GdbSessionConfig()
        sessionConfig.remote_host = component.get("hostname", None)
        sessionConfig.remote_port = component.get("remote_port", 0)
        sessionConfig.gdb_mode = GdbMode.REMOTE if "mode" in component.keys(
        ) and component["mode"] == "remote" else GdbMode.LOCAL
        if sessionConfig.gdb_mode == GdbMode.REMOTE:
            remote_cred = SSHRemoteServerCred(
                hostname=component["cred"]["hostname"],
                username=component["cred"]["user"],
            )
            sessionConfig.remote_gdbserver = SSHRemoteSeverClient(
                cred=remote_cred)
        sessionConfig.tag = component.get("tag", None)
        sessionConfig.start_mode = component.get("startMode", StartMode.Binary)
        sessionConfig.attach_pid = component.get("pid", 0)
        sessionConfig.binary = component.get("bin", None)
        sessionConfig.cwd = component.get("cwd", ".")
        sessionConfig.args = component.get("args", [])
        sessionConfig.run_delay = component.get("run_delay", 0)
        gdbSessionConfigs.append(sessionConfig)
    gdb_manager = GdbManager(gdbSessionConfigs)

    # del gdb_manager
    # gdbmi = GdbController(["gdb", "./bin/hello_world", "--interpreter=mi"])
    # print(gdbmi.command)  # print actual command run as subprocess
    # for response in gdbmi.get_gdb_response():
    #     print_resp(response)
    #     pprint(response)

    while True:
        cmd = input("(gdb) ").strip()
        cmd = f"{cmd}\n"
        gdb_manager.write(cmd)
        # cmd_head = cmd.split()[0]

        # if cmd_head in ["break", "b", "-break-insert"]:
        #     # gdbmi.write
        #     gdb_manager.write(cmd)
        # else:
        #     responses = gdbmi.write(cmd)
        #     for response in responses:
        #         print_resp(response)
        #         pprint(response)


def exec_cmd(cmd: Union[List[str], str]):
    if isinstance(cmd, str):
        cmd = [cmd]
    result = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE
    )
    print(result.stdout.decode("utf-8"))
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

    print(f"Executing task: {name}, command: {command}")
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


def bootFromNuConfig():
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

    with open(str(args.config), "r") as fs:
        try:
            config_data = safe_load(fs)
            print("Loaded dbg config file:")
            pprint(config_data)
        except YAMLError as e:
            eprint(f"Failed to read the debugging config. Error: {e}")

    if not config_data:
        eprint("Debugging config is required!")
        exit(1)

    exec_pretasks(config_data)

    gdb_manager: GdbManager = None
    try:
        main()
    except KeyboardInterrupt:
        print(f"Received interrupt")

        if gdb_manager:
            gdb_manager.cleanup()

        exec_posttasks(config_data)

        try:
            sys.exit(130)
        except SystemExit:
            os._exit(130)


def bootServiceWeaverKube():
    kubeconfig.load_incluster_config()
    clientset = kubeclient.CoreV1Api()
    global gdb_manager, config_data

    kube_namespace = "default"
    sw_name = "serviceweaver1"
    selector_label = f"serviceweaver/app={sw_name}"
    pods = clientset.list_namespaced_pod(
        namespace=kube_namespace, label_selector=selector_label)
    gdbSessionConfigs: List[GdbSessionConfig] = []
    for i in pods.items:
        print("%s\t%s\t%s" %
              (i.status.pod_ip, i.metadata.namespace, i.metadata.name))
        remoteServerConn = KubeRemoteSeverClient(
            i.metadata.name, i.metadata.namespace)
        remoteServerConn.connect()
        output = remoteServerConn.execute_command(['ps', '-eo', "pid,comm"])
        # Use a regular expression to find the PID for 'serviceweaver1'
        match = re.search(r'(\d+)\s+{}'.format(sw_name), output)
        if match:
            pid = match.group(1)
            sessionConfig= GdbSessionConfig()
            sessionConfig.remote_port=30001
            sessionConfig.remote_host=i.status.pod_ip
            sessionConfig.gdb_mode=GdbMode.REMOTE
            sessionConfig.remote_gdbserver=remoteServerConn
            sessionConfig.tag=i.metadata.name
            sessionConfig.start_mode=StartMode.ATTACH
            sessionConfig.attach_pid=int(pid)
            gdbSessionConfigs.append(sessionConfig)
        else:
            eprint(i.status.pod_ip, i.metadata.name,
                   "cannot locate service weaver process:", sw_name)

    gdb_manager = GdbManager(gdbSessionConfigs)

    # del gdb_manager
    # gdbmi = GdbController(["gdb", "./bin/hello_world", "--interpreter=mi"])
    # print(gdbmi.command)  # print actual command run as subprocess
    # for response in gdbmi.get_gdb_response():
    #     print_resp(response)
    #     pprint(response)

    while True:
        cmd = input("(gdb) ").strip()
        cmd = f"{cmd}\n"
        print(cmd)
        gdb_manager.write(cmd)
        # cmd_head = cmd.split()[0]

        # if cmd_head in ["break", "b", "-break-insert"]:
        #     # gdbmi.write
        #     gdb_manager.write(cmd)
        # else:
        #     responses = gdbmi.write(cmd)
        #     for response in responses:
        #         print_resp(response)
        #         pprint(response)


if __name__ == "__main__":
    bootServiceWeaverKube()
    pass
