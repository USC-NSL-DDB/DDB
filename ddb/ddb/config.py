import os
from pprint import pprint, pformat
from yaml import YAMLError, safe_load
from typing import List, Optional

from ddb.gdbserver_starter import SSHRemoteServerClient, SSHRemoteServerCred
from ddb.utils import eprint
from ddb.data_struct import BrokerInfo, DDBConfig, GdbMode, GdbSessionConfig, StartMode, TargetFramework
from ddb.logging import logger

class GlobalConfig:
    __global_config = DDBConfig()

    @staticmethod
    def get() -> DDBConfig:
        '''Just a alias'''
        return GlobalConfig.get_config()

    @staticmethod
    def set(config: DDBConfig):
        '''Just a alias'''
        GlobalConfig.set_config(config)

    @staticmethod
    def get_config() -> DDBConfig:
        return GlobalConfig.__global_config

    @staticmethod
    def set_config(config: DDBConfig):
        # logger.debug(f"Setting global config: \n{pformat(config)}")
        GlobalConfig.__global_config = config

    @staticmethod
    def parse_nu_config(ddb_config: DDBConfig, config_data: any):
        service_discovery_enabled = ("ServiceDiscovery" in config_data)
        if service_discovery_enabled:
            broker_info = config_data["ServiceDiscovery"]["Broker"]
            ddb_config.broker = BrokerInfo(
                broker_info["hostname"],
                broker_info["port"]
            ) 
    
        gdbSessionConfigs: List[GdbSessionConfig] = []
        components = config_data["Components"] if "Components" in config_data else []
        # TODO: use prerun commands. For now, just ignore it.
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
                    cred=remote_cred)

            gdbSessionConfigs.append(sessionConfig)
        ddb_config.gdb_sessions_configs = gdbSessionConfigs

    @staticmethod
    def parse_config_file(config_data: any) -> DDBConfig:
        ddb_config = DDBConfig()

        if "Framework" in config_data:
            if config_data["Framework"] == "serviceweaver_kube":
                ddb_config.framework = TargetFramework.SERVICE_WEAVER_K8S
                # parse_serviceweaver_kube_config(ddb_config, config_data)
            elif config_data["Framework"] == "Nu":
                ddb_config.framework = TargetFramework.NU
                GlobalConfig.parse_nu_config(ddb_config, config_data)
            else:
                ddb_config.framework = TargetFramework.UNSPECIFIED
                # TODO: parse a configuration file for a unspecified framework
                GlobalConfig.parse_nu_config(ddb_config, config_data)
        else:
            ddb_config.framework = TargetFramework.UNSPECIFIED
            # TODO: parse a configuration file for a unspecified framework
            GlobalConfig.parse_nu_config(ddb_config, config_data)

        return ddb_config

    @staticmethod
    def load_config(path: Optional[str]) -> bool:
        config_data = None
        if path is not None:
            with open(str(path), "r") as fs:
                try:
                    config_data = safe_load(fs)
                    logger.info(f"Loaded dbg config file: \n{pformat(config_data)}")
                    # eprint("Loaded dbg config file:")
                    # Set parsed config to the global scope
                    GlobalConfig.set_config(GlobalConfig.parse_config_file(config_data))
                except YAMLError as e:
                    eprint(f"Failed to read the debugging config. Error: {e}")
                    return False
        else:
            eprint("Config file path is not specified...")
            return False
        return True


# class GlobalConfig:
#     __config: Optional[DDBConfig] = None
#     __lock = Lock()

#     def __init__(self) -> None:
#         pass


#     @staticmethod
#     def SetConfig(config: DDBConfig):
#         with GlobalConfig.__lock:
#             GlobalConfig.__config = config

