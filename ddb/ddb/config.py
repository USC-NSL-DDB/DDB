import os
from pprint import pprint
from threading import Lock
from typing import List, Optional

from yaml import YAMLError, safe_load

from ddb.gdbserver_starter import SSHRemoteServerClient, SSHRemoteServerCred
from ddb.utils import eprint
from ddb.data_struct import BrokerInfo, DDBConfig, GdbMode, GdbSessionConfig, StartMode

class GlobalConfig:
    _config: DDBConfig = None
    _lock = Lock()
    
    def __init__(self, config: DDBConfig) -> None:
        with GlobalConfig._lock:
            if GlobalConfig._config:
                return
            GlobalConfig._config = config

    def get() -> Optional[DDBConfig]:
        with GlobalConfig._lock:
            return GlobalConfig._config

def LoadConfig(file_path: str) -> bool:
    config_data = None
    with open(file_path, "r") as fs:
        try:
            config_data = safe_load(fs)
            eprint("Loaded dbg config file:")
            pprint(config_data)
        except YAMLError as e:
            eprint(f"Failed to read the debugging config. Error: {e}")
            return False

    gdbSessionConfigs: List[GdbSessionConfig] = []
    broker_info: BrokerInfo = None
    prerun_cmds = None

    components = config_data["Components"] if "Components" in config_data else []
    prerun_cmds = config_data["PrerunGdbCommands"] if "PrerunGdbCommands" in config_data else None

    for component in components:
        sessionConfig = GdbSessionConfig()

        sessionConfig.tag = component.get("tag", None)
        sessionConfig.start_mode = component.get("startMode", StartMode.BINARY)
        sessionConfig.attach_pid = component.get("pid", 0)
        sessionConfig.binary = component.get("bin", None)
        sessionConfig.cwd = component.get("cwd", os.getcwd())
        sessionConfig.args = component.get("args", [])
        sessionConfig.run_delay = component.get("run_delay", 0)
        sessionConfig.sudo = component.get("sudo", False)

        sessionConfig.gdb_mode = GdbMode.REMOTE if \
            "mode" in component.keys() and component["mode"] == "remote" \
            else GdbMode.LOCAL
        if sessionConfig.gdb_mode == GdbMode.REMOTE:
            sessionConfig.remote_port = component["remote_port"]
            sessionConfig.remote_host = component["cred"]["hostname"]
            sessionConfig.username = component["cred"]["user"]
            remote_cred = SSHRemoteServerCred(
                port=sessionConfig.remote_port,
                bin=os.path.join(sessionConfig.cwd, sessionConfig.binary), # respect current working directoy.
                hostname=sessionConfig.remote_host,
                username=sessionConfig.username
            )
            sessionConfig.remote_gdbserver = SSHRemoteServerClient(
                cred=remote_cred
            )

        gdbSessionConfigs.append(sessionConfig)
        
    if "ServiceDiscovery" in config_data:
        sd = config_data["ServiceDiscovery"]
        broker_info = sd["Broker"]
        broker_info = BrokerInfo(broker_info["hostname"], broker_info["port"])

    # GlobalConfig = DDBConfig(gdb_sessions_configs=gdbSessionConfigs, broker=broker_info) 
    GlobalConfig(
        DDBConfig(gdb_sessions_configs=gdbSessionConfigs, broker=broker_info)
    )
            
    return True
