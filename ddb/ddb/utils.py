import os
import shutil
import subprocess
import sys
from threading import Lock
import time
from typing import Tuple

import pkg_resources

from ddb.const import ServiceDiscoveryConst
from ddb.counter import TSCounter
from ddb.data_struct import BrokerInfo
from ddb.logging import logger

def folder_struct_setup():
    folders = [
        "/tmp/ddb",
        "/tmp/ddb/mosquitto/",
        "/tmp/ddb/logs/mosquitto/",
        "/tmp/ddb/service_discovery/"
    ]

    for folder in folders:
        if not os.path.exists(folder):
            os.makedirs(folder)
            logger.debug(f"Created folder: {folder}")

    broker_config = pkg_resources.resource_filename('ddb', 'conf/mosquitto.conf')
    destination_file = "/tmp/ddb/mosquitto/mosquitto.conf"
    shutil.copy(broker_config, destination_file)

def start_mosquitto_broker(broker: BrokerInfo):
    folder_struct_setup()
    with open(ServiceDiscoveryConst.SERVICE_DISCOVERY_INI_FILEPATH, 'w') as f:
        f.writelines(
            [
                f"{ServiceDiscoveryConst.BROKER_MSG_TRANSPORT}://{broker.hostname}:{broker.port}\n",
                f"{ServiceDiscoveryConst.T_SERVICE_DISCOVERY}\n",
            ]
        )
    try:
        subprocess.Popen(["mosquitto", "-c", "/tmp/ddb/mosquitto/mosquitto.conf", "-d"]) # run mosquitto broker in daemon mode
        logger.debug("Mosquitto broker started successfully!")
    except FileNotFoundError:
        logger.error("Mosquitto program not found. Please make sure it is installed.")
    except Exception as e:
        logger.error(f"Failed to start Mosquitto broker: {e}")

    logger.debug("Waiting 5s for broker to start...")
    time.sleep(5) # wait for the broker to start

def cleanup_mosquitto_broker():
    try:
        if shutil.which("sudo"):
            subprocess.run(["sudo", "pkill", "mosquitto"])
        else:
            subprocess.run(["pkill", "mosquitto"])
        logger.debug("Mosquitto broker terminated successfully!")
    except Exception as e:
        logger.error(f"Failed to terminate Mosquitto broker: {e}")

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
