from abc import ABC, abstractmethod
from time import sleep
from kubernetes import config as kubeconfig, client as kubeclient
from kubernetes.stream import stream
from kubernetes.client.rest import ApiException
import uuid
from ddb.logging import logger


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

class ServiceWeaverkubeGdbController(RemoteGdbController):
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
            image="debuggerimage:latest",
            target_container_name=self.target_container_name,
            image_pull_policy="IfNotPresent",
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
        std_err=self.resp.read_stderr(timeout)
        if self.verbose and std_err:
            logger.debug(f"<<---(error)Receive error from[{self.pod_name}] [{std_err}] ")
        if self.verbose and std_output:
            logger.debug(f"<<-------------Receive output from[{self.pod_name}] [{std_output}] ")
        return std_output.encode()
    def is_open(self) -> bool:
        return hasattr(self, 'resp') and self.resp.is_open()
    def close(self):
        self.resp.close()