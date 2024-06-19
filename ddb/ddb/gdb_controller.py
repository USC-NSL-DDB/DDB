from abc import ABC, abstractmethod
from time import sleep
from kubernetes import config as kubeconfig, client as kubeclient
from kubernetes.stream import stream
from kubernetes.client.rest import ApiException
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
    def __init__(self, pod_name: str, pod_namespace: str,verbose=False):
        self.pod_name = pod_name
        self.pod_namespace = pod_namespace
        self.api_instance=kubeclient.CoreV1Api()
        self.verbose=verbose
        try:
            resp = self.api_instance.read_namespaced_pod(name=pod_name,
                                                    namespace='default')
        except ApiException as e:
            print(f"fail to find pod with the given name: {e} {pod_name} {pod_namespace}")
        exec_command = [
        '/bin/sh',
        '-c',
        'echo This message goes to stderr; echo This message goes to stdout']
        resp = stream(self.api_instance.connect_get_namespaced_pod_exec,
                  self.pod_name,
                  self.pod_namespace,
                  command=exec_command,
                  stderr=True, stdin=False,
                  stdout=True, tty=False)
        print(f"Response from pod ${self.pod_name}: " + resp)
        exec_command = ['gdb','--interpreter=mi3','-q']
        self.resp = stream(self.api_instance.connect_get_namespaced_pod_exec,
                        self.pod_name,
                        self.pod_namespace,
                        command=exec_command,
                        stderr=True, stdin=True,
                        stdout=True, tty=False,
                        _preload_content=False)
    def start(self,command):
        # if self.verbose:
        #     print(f"------------->>Send input to [{self.pod_name}] [{command}] ")
        # sleep(1)
        # self.resp.write_stdin(f"{command}\n")
        pass
    def write_input(self,command):
        if self.verbose:
            print(f"------------->>Send input to [{self.pod_name}] [{command}] ")
        self.resp.write_stdin(f"{command}\n")
    def fetch_output(self,timeout=1):
        std_output=self.resp.read_stdout(timeout)
        std_err=self.resp.read_stderr(timeout)
        if self.verbose and std_err:
            print(f"<<---(error)Receive error from[{self.pod_name}] [{std_err}] ")
        if self.verbose and std_output:
            print(f"<<-------------Receive output from[{self.pod_name}] [{std_output}] ")
        return std_output.encode()
    def is_open(self) -> bool:
        return self.resp.is_open()
    def close(self):
        self.resp.close()