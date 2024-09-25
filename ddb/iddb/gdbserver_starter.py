import threading
from typing import List, Optional
import paramiko
from dataclasses import dataclass
from abc import ABC, abstractmethod
from iddb.logging import logger

class RemoteServerConnection(ABC):
    @abstractmethod
    def start(self, command: str):
        pass

    @abstractmethod
    def write(self, command: str):
        pass

    @abstractmethod
    def readline(self) -> str:
        pass

    @abstractmethod
    def close(self):
        pass
    

@dataclass
class SSHRemoteServerCred:
    port: str
    # bin: str
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

    ''' New API for non-gdbserver impl
    '''
    def start(self, command: str) -> bool:
        # connect ssh first
        self.client.connect(self.cred.hostname, username=self.cred.username)

        # TODO: need to setup remote environment first...
        # such as cp'ing extension scripts
        stdin, stdout, stderr = self.client.exec_command(command)
        self.stdin = stdin
        self.stdout = stdout
        self.stderr = stderr

    def readline(self) -> str:
        ''' raise socket.error if connection is closed
        '''
        return self.stdout.readline()

    def write(self, command: str):
        self.stdin.write(command.strip() + "\n")
        self.stdin.flush()
        
    def close(self):
        if self.client:
            self.client.close()
        if self.stdin:
            self.stdin.close()
        if self.stdout:
            self.stdout.close()
        if self.stderr:
            self.stderr.close()

# class KubeRemoteSeverClient(RemoteServerConnection):
#     from kubernetes import config as kubeconfig, client as kubeclient, stream

#     def __init__(self, pod_name: str, pod_namespace: str):
#         self.pod_name = pod_name
#         self.pod_namespace = pod_namespace

#     def connect(self):
#         pass

#     def execute_command(self, command):
#         self.clientset=KubeRemoteSeverClient.kubeclient.CoreV1Api()
#         output = KubeRemoteSeverClient.stream.stream(self.clientset.connect_get_namespaced_pod_exec, self.pod_name, self.pod_namespace,
#                                command=command, stderr=True, stdin=False,
#                                stdout=True, tty=False)
#         return output
#     def close(self):
#         pass
#     def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
#         pass


# class LocalClient(RemoteServerConnection):
#     def __init__(self):
#         pass

#     def connect(self):
#         pass

#     def execute_command(self, command):
#         import subprocess
#         subprocess.run(command)

#     def close(self):
#         pass

#     def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
#         pass