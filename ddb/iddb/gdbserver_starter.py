import asyncio
import threading
from typing import List, Optional
import asyncssh
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
    def __init__(self, cred: SSHRemoteServerCred, private_key_path=None, max_retries=5, base_delay=0.5, backoff_factor=2):
        self.cred = cred
        self.private_key_path = private_key_path or '~/.ssh/id_rsa'
        self.conn = None
        self.process = None
        # Automatic retry logic for SSH connection
        self.max_retries = max_retries
        self.base_delay = base_delay
        self.backoff_factor = backoff_factor

    async def connect(self) -> bool:
        connected = False
        attempt = 0
        while attempt < self.max_retries:
            try:
                self.conn = await asyncssh.connect(
                    self.cred.hostname, 
                    username=self.cred.username,
                    client_keys=[self.private_key_path],
                    connect_timeout=10
                )
                connected = True
                break
            except (asyncssh.Error, OSError) as e:
                attempt += 1
                # exponential backoff
                delay = self.base_delay * (self.backoff_factor ** attempt)
                logger.info(f"Failed to connect to {self.cred.hostname}, retrying in {delay} seconds")
                await asyncio.sleep(delay)
        if not connected:
            logger.error(f"Failed to connect to {self.cred.hostname} after {self.max_retries} attempts")
            raise RuntimeError(f"Failed to connect to {self.cred.hostname} after {self.max_retries} attempts")
        return connected

    async def start(self, command: str) -> bool:
        ''' Start the debugger process on the remote server.
        Returns True if the connection is successful, False otherwise.
        If the connection is unsuccessful, the process will not be started.
        The caller should carefully cleanup the state by invoking close() method.

        TODO: 
        ATM, no-retry will be attempted if the connection is lost after the first connection.
        When the SSH connection is lost and re-established, we lost the states of the previous connection.
        Thus, the previous debugger session is lost and we need more careful thought regarding how to handle this case.
        '''
        # connect ssh first
        connected = await self.connect()

        if connected:
            # Start the debugger process
            self.process = await self.conn.create_process(command)
        return connected

    async def readline(self) -> str:
        '''raise socket.error if connection is closed'''
        if not self.process:
            raise RuntimeError("Process not started")
        return await self.process.stdout.readline()

    def write(self, command: str):
        if not self.process:
            raise RuntimeError("Process not started")
        self.process.stdin.write(command.strip() + "\n")

    async def close(self):
        if self.process:
            self.process.close()
            self.process = None
        if self.conn:
            self.conn.close()
            await self.conn.wait_closed()
            self.conn = None

class KubeRemoteSeverClient(RemoteServerConnection):
    from kubernetes import config as kubeconfig, client as kubeclient, stream

    def __init__(self, pod_name: str, pod_namespace: str,target_container_name:str):
        self.pod_name = pod_name
        self.pod_namespace = pod_namespace
        self.target_container_name=target_container_name

    def connect(self):
        pass

    def execute_command(self, command):
        self.clientset=KubeRemoteSeverClient.kubeclient.CoreV1Api()
        output = KubeRemoteSeverClient.stream.stream(self.clientset.connect_get_namespaced_pod_exec, self.pod_name, self.pod_namespace, 
                               command=command, stderr=True, stdin=False,
                               stdout=True, tty=False)
        return output
    def readline(self):
        pass
    def write(self):
        pass
    def close(self):
        pass
    def start(self, args: Optional[List[str]] = None, attach_pid: Optional[int] = None, sudo: bool = False):
        pass


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