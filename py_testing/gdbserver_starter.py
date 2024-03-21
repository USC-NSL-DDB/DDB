import threading
from typing import List, Optional
import paramiko
from dataclasses import dataclass
from abc import ABC, abstractmethod
from kubernetes import client, config, stream
import kubernetes


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
class RemoteServerCred:
    remote_port: str 
    hostname: str
    username: str

class SSHRemoteSeverClient(RemoteServerConnection):

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
        return stdout.read()

    def close(self):
        if self.client:
            self.client.close()


class KubeRemoteSeverClient(RemoteServerConnection):
    def __init__(self, pod_name: str, pod_namespace: str):
        self.pod_name = pod_name
        self.pod_namespace = pod_namespace

    def connect(self):
        pass

    def execute_command(self, command):
        config.load_incluster_config()
        self.clientset=kubernetes.client.CoreV1Api()
        output = stream.stream(self.clientset.connect_get_namespaced_pod_exec, self.pod_name, self.pod_namespace,
                               command=command, stderr=True, stdin=False,
                               stdout=True, tty=False)
        return output
    def close(self):
        pass



class RemoteGdbServer:
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

    def start(self, args: Optional[List[str]] = None):
        command = f"gdbserver :{self.cred.remote_port} {self.cred.bin} {' '.join(args) if args else ''}"
        stdin, stdout, stderr = self.client.exec_command(command)
        # You can handle the output and error streams here if needed
        print("Start gdbserver on remote machine... Response:")

        # line = stdout.readline(10)
        # print(line)
        # for line in stdout.readlines(10):
        #     print(line)

    def disconnect(self):
        self.client.close()
