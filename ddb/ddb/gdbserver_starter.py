import threading
from typing import List, Optional
import paramiko
from dataclasses import dataclass
from abc import ABC, abstractmethod
from ddb.logging import logger

class RemoteServerConnection(ABC):
    @abstractmethod
    def connect(self):
        pass

    @abstractmethod
    def execute_command(self, command: List[str]):
        pass

    def execute_command_async(self,command: List[str]):
        threading.Thread(target=lambda:self.execute_command(command)).start()

    @abstractmethod
    def close(self):
        pass
    

@dataclass
class SSHRemoteServerCred:
    port: str
    bin: str
    hostname: str
    username: str


class SSHRemoteServerClient(RemoteServerConnection):

    def __init__(self, cred: SSHRemoteServerCred, private_key_path=None):
        self.cred = cred

        if private_key_path is None:
            self.private_key_path = '~/.ssh/id_rsa'
        else:
            self.private_key_path = private_key_path

        self.client = paramiko.SSHClient()
        self.client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        self.client.load_system_host_keys()
        self.client.connect(self.cred.hostname, username=self.cred.username)

    def connect(self):
        self.client = paramiko.SSHClient()
        self.client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        self.client.load_system_host_keys()
        self.client.connect(self.cred.hostname, username=self.cred.username)

    def execute_command(self, command):
        stdin, stdout, stderr = self.client.exec_command(' '.join(command))
        # return stdout.read()

    def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
        from ddb.config import DevFlags

        command = None
        if DevFlags.USE_EXTENDED_REMOTE:
            # sudo is forcely used. Attaching to running process with pid required sudo permission.
            command = f"sudo gdbserver --multi :{self.cred.port}"
        else:
            if attach_pid and isinstance(attach_pid, int):
                command = f"gdbserver :{self.cred.port} --attach {str(attach_pid)}"
            else:
                command = f"gdbserver :{self.cred.port} {self.cred.bin} {' '.join(args) if args else ''}"
            if sudo:
                command = f"sudo {command}"

        command = f"{command} > /tmp/gdbserver.log 2>&1"
        logger.debug(f"Starting gdbserver on remote machine... \n\targs: {args}, \n\tattach_pid: {attach_pid}, \n\tsudo: {sudo}, \n\tcommand: {command}")
        stdin, stdout, stderr = self.client.exec_command(command)
        # You can handle the output and error streams here if needed
        logger.debug(f"Finished starting gdbserver on remote machine...")

    def close(self):
        if self.client:
            self.client.close()


class KubeRemoteSeverClient(RemoteServerConnection):
    from kubernetes import config as kubeconfig, client as kubeclient, stream

    def __init__(self, pod_name: str, pod_namespace: str):
        self.pod_name = pod_name
        self.pod_namespace = pod_namespace

    def connect(self):
        pass

    def execute_command(self, command):
        self.clientset=KubeRemoteSeverClient.kubeclient.CoreV1Api()
        output = KubeRemoteSeverClient.stream.stream(self.clientset.connect_get_namespaced_pod_exec, self.pod_name, self.pod_namespace,
                               command=command, stderr=True, stdin=False,
                               stdout=True, tty=False)
        return output
    def close(self):
        pass
    def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
        pass
class LocalClient(RemoteServerConnection):
    def __init__(self):
        pass

    def connect(self):
        pass

    def execute_command(self, command):
        import subprocess
        subprocess.run(command)

    def close(self):
        pass

    def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
        pass