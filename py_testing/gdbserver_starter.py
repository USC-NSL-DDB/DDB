from typing import List, Optional
import paramiko
from dataclasses import dataclass


@dataclass
class RemoteServerCred:
    remote_port: str
    hostname: str
    username: str
    bin: str

    def __init__(self, remote_port: str, hostname: str, username: str, bin: str):
        self.remote_port = remote_port
        self.hostname = hostname
        self.username = username
        self.bin = bin


class RemoteGdbServer:
    def __init__(self, cred: RemoteServerCred, private_key_path=None):
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
