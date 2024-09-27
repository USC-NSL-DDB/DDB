
import sys
from threading import Lock
from typing import Tuple

from iddb.counter import TSCounter

def eprint(*args, **kwargs):
    dev_print(*args, **kwargs)

def mi_print(response, meta: str):
    try:
        token = None
        if "token" in response:
            token = response["token"]

        type = response["type"]
        if type in [ "console", "output", "notify", "result" ]:
            msg = response["message"]
            payload = response["payload"] 
            out = f"\n{meta} [ type: {type}, token: {token}, message: {msg} ]\n{payload}\n" 
            if response["stream"] == "stdout":
                dev_print(out, end="")
            if response["stream"] == "stderr":
                eprint(out, end="")
    except Exception as e:
        dev_print(f"response: {response}. meta: {meta}, e: {e}")

def wrap_grouped_message(msg: str) -> str:
    return f"**** GROUPED RESPONSE START ****\n{msg}\n**** GROUPED RESPONSE END ****\n\n"

# A simple wrapper around counter in case any customization later
''' Generate a global unique/incremental token for every cmd it sends
'''
class CmdTokenGenerator:
    _sc: "CmdTokenGenerator" = None
    _lock = Lock()

    def __init__(self) -> None:
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "CmdTokenGenerator":
        with CmdTokenGenerator._lock:
            if CmdTokenGenerator._sc:
                return CmdTokenGenerator._sc
            CmdTokenGenerator._sc = CmdTokenGenerator()
            return CmdTokenGenerator._sc

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return str(CmdTokenGenerator.inst().inc())

trace = True

def dev_print(*args, **kwargs):
    if trace:
        print(*args, file=sys.stderr, **kwargs)

def parse_cmd(cmd: str) -> Tuple[str, str, str, str]:
    """
    Parses a gdb command string and returns a tuple containing the token, command without token,
    prefix, and the original command string.

    Args:
        cmd (str): The command string to be parsed.

    Returns:
        tuple: A tuple containing the token, command without token, prefix, and the original command string.
    """
    token = None
    cmd_no_token = None
    prefix = None
    cmd = cmd.strip()
    for idx, cmd_char in enumerate(cmd):
        if (not cmd_char.isdigit()) and (idx == 0):
            prefix = cmd.split()[0]
            cmd_no_token = cmd
            break
        
        if not cmd_char.isdigit():
            token = cmd[:idx].strip()
            cmd_no_token = cmd[idx:].strip()
            if len(cmd_no_token) == 0:
                # no meaningful input
                return (None, None, None)
            prefix = cmd_no_token.split()[0]
            break
    return (token, cmd_no_token, prefix, f"{cmd}\n")

def ip_str2ip_int(ip_str: str) -> int:
    """
    Convert an IP string to int
    """
    import socket, struct
    packed_ip = socket.inet_aton(ip_str)
    return struct.unpack("!L", packed_ip)[0]

def ip_int2ip_str(ip_int: int) -> str:
    """
    Convert an IP int to str 
    """
    import socket, struct
    return socket.inet_ntoa(struct.pack('!L', ip_int))
