from abc import ABC, abstractmethod
import subprocess
from time import sleep
import time
from typing import Optional
from kubernetes import config as kubeconfig, client as kubeclient
from kubernetes.stream import stream
from kubernetes.client.rest import ApiException
import uuid
from iddb.gdbserver_starter import SSHRemoteServerClient, SSHRemoteServerCred
from iddb.logging import logger


class RemoteGdbController(ABC):
    @abstractmethod
    def start(self,command):
        """
        connect to server and start gdb with the given command
        """
        pass
    @abstractmethod
    def write_input(self,command):
        """
        fetch output with optional timeout
        """
        pass
    @abstractmethod
    def fetch_output(self,timeout=1)->bytes:
        """
        fetch output with optional timeout
        """
        pass
    @abstractmethod
    def is_open(self)->bool:
        pass
    @abstractmethod
    def close(self):
        pass

class VanillaPIDController():
    def __init__(self, pid: int, verbose=False):
        self.pid = pid
        self.verbose = verbose
        self.process = None

    def start(self, command: str):
        if self.verbose:
            logger.debug(f"Starting GDB for process {self.pid}")
        self.process = subprocess.Popen(
            ['gdb', '--interpreter=mi3', '-q'],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=0
        )
        # Wait for GDB to initialize
        time.sleep(1)

    def write_input(self, command):
        if isinstance(command, list):
            command = "\n".join(command)
        if self.verbose:
            logger.debug(f"Sending input to {self.pid}: {command}")
        self.process.stdin.write(f"{command}\n")
        self.process.stdin.flush()

    def fetch_output(self, timeout=1):
        # start_time = time.time()
        line = self.process.stdout.readline()

        if self.verbose and line:
            logger.debug(f"Received output from {self.pid}: {line}")
        return line.encode()

    def is_open(self) -> bool:
        return self.process is not None and self.process.poll() is None

    def close(self):
        if self.is_open():
            if self.verbose:
                print(f"Closing GDB for process {self.pid}")
            self.write_input("quit")
            time.sleep(0.5)
            if self.is_open():
                self.process.terminate()
                time.sleep(0.5)
                if self.is_open():
                    self.process.kill()
            self.process = None

class ServiceWeaverkubeGdbController(RemoteGdbController):
    count=0
    def __init__(self, pod_name: str, pod_namespace: str,target_container_name:str,verbose=False):
        self.pod_name = pod_name
        self.pod_namespace = pod_namespace
        self.target_container_name=target_container_name
        self.api_instance=kubeclient.CoreV1Api()
        # Generate a random UUID
        self.debugger_container_name=f"debugger-ephemeral{str(uuid.uuid4())}"
        self.verbose=verbose
        try:
            resp = self.api_instance.read_namespaced_pod(name=pod_name,
                                                    namespace='default')
            container_names=[]
            for container in resp.spec.containers:
                container_names.append(container.name)
            if target_container_name not in container_names:
                raise Exception("No such container in the target pod")
        except ApiException as e:
            print(f"fail to find pod with the given name: {e} {pod_name} {pod_namespace}")
        # create ephemeral container
        # Add a debug container to it
        debug_container = kubeclient.V1EphemeralContainer(
            name=self.debugger_container_name,
            image="h21565897/debuggerimage:latest2",
            target_container_name=self.target_container_name,
            image_pull_policy="Always",
            stdin=True,
            tty=False
        )
        patch_body = {
            "spec": {
                "ephemeralContainers": [
                    debug_container
                ]
            }
        }
        self.api_instance.patch_namespaced_pod_ephemeralcontainers(
            name=self.pod_name,
            namespace=self.pod_namespace,
            body=patch_body
        )
    def start(self,command:str):
        # maybe this command should synchronouly start the gdb
        # stuck until it starts successfully
        while True:
            pod = self.api_instance.read_namespaced_pod(name=self.pod_name, namespace=self.pod_namespace)
            containers = pod.status.ephemeral_container_statuses
            if containers and any(c.name == self.debugger_container_name and c.state.running for c in containers):
                print(f"Ephemeral container {self.debugger_container_name} is now running.")
                break
            sleep(1)
        self.resp = stream(
            self.api_instance.connect_get_namespaced_pod_exec,
            name=self.pod_name,
            namespace=self.pod_namespace,
            command=['gdb','--interpreter=mi3','-q'],
            container=self.debugger_container_name,
            stderr=True, stdin=True,
            stdout=True, tty=False,
            _preload_content=False
        )
    def write_input(self,command):
        if self.verbose:
            logger.debug(f"------------->>Send input to [{self.pod_name}] [{command}] ")
        self.resp.write_stdin(f"{command}\n")
    
    def fetch_output(self,timeout=1):
        std_output=self.resp.read_stdout(timeout)
        # std_err=self.resp.read_stderr(timeout)
        # if self.verbose and std_err:
        #     logger.debug(f"<<---(error)Receive error from[{self.pod_name}] [{std_err}] ")
        if self.verbose and std_output:
            logger.debug(f"<<-------------Receive output from[{self.pod_name}] [{std_output}] ")
        return std_output.encode()
    def is_open(self) -> bool:
        return hasattr(self, 'resp') and self.resp.is_open()
    def close(self):
        self.resp.close()

class SSHAttachController(RemoteGdbController):
    def __init__(self, pid: int, cred: SSHRemoteServerCred, verbose: bool = False):
        self.pid = pid
        self.verbose = verbose
        self.client = SSHRemoteServerClient(cred)
        self.cred = cred
        self.open = False

    def start(self, command: str):
        if self.verbose:
            logger.debug(f"Starting {str(self)}")
        try:
            self.client.start("gdb --interpreter=mi3 -q")
        except Exception as e:
            raise e # TODO: wrap exceptino to an internal data structure
        self.open = True
        # TODO: should we wait a bit to more gracefully handle potential startup error?
        # FIXME: figure out a way to pass back stderr
    
    def write_input(self, command: str):
        if self.verbose:
            logger.debug(f"Sending input to {str(self)}: {command}")
        self.client.write(command)
    
    def fetch_output(self, timeout=1) -> bytes:
        return self.client.readline().encode()
    
    def is_open(self) -> bool:
        return self.open 
    
    def close(self):
        self.client.close()
        self.open = False

    def __str__(self):
        return f"GDBController-SSH-(pid={self.pid}, cred={self.cred})"
