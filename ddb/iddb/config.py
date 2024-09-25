import os
import re
from iddb.gdb_controller import ServiceWeaverkubeGdbController, VanillaPIDController, SSHAttachController
from yaml import YAMLError, safe_load
from typing import List, Optional
from pprint import pformat
import getpass

from iddb.gdbserver_starter import SSHRemoteServerCred
from iddb.data_struct import BrokerInfo, DDBConfig, GdbMode, GdbSessionConfig, StartMode, TargetFramework
from iddb.logging import logger
from iddb.const import ServiceDiscoveryConst

class DevFlags:
    pass
    # USE_EXTENDED_REMOTE = True

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
                ServiceDiscoveryConst.BROKER_PORT
            ) 

        ssh_provided = ("SSH" in config_data)
        if ssh_provided:
            ssh_info = config_data["SSH"]
            ddb_config.ssh.user = ssh_info.get("user", getpass.getuser())
            ddb_config.ssh.port = ssh_info.get("port", 22)
    
        gdbSessionConfigs: List[GdbSessionConfig] = []
        components = config_data["Components"] if "Components" in config_data else []
        prerun_cmds = config_data["PrerunGdbCommands"] if "PrerunGdbCommands" in config_data else []

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
            sessionConfig.prerun_cmds = prerun_cmds

            sessionConfig.gdb_mode = GdbMode.REMOTE if \
                "mode" in component.keys() and component["mode"] == "remote" \
                else GdbMode.LOCAL
            if sessionConfig.gdb_mode == GdbMode.REMOTE:
                # TODO: implement a controller that run binary instead of attach pid
                pass
                # sessionConfig.gdb_controller = SSHAttachController(
                #     pid=sessionConfig.attach_pid,
                #     cred=SSHRemoteServerCred(
                #         port=sessionConfig.remote_port,
                #         hostname=sessionConfig.remote_host,
                #         username=sessionConfig.username
                #     ),
                #     verbose=True
                # )

            gdbSessionConfigs.append(sessionConfig)
        ddb_config.gdb_sessions_configs = gdbSessionConfigs

    @staticmethod
    def parse_serviceweaver_kube_config(ddb_config: DDBConfig, config_data: any):
        from kubernetes import config as kubeconfig, client as kubeclient
        from iddb.gdbserver_starter import KubeRemoteSeverClient
        try:
            kubeconfig.load_kube_config()
        except Exception as e:
            print("fail to fetch cluster information")
            exit(0)
        clientset = kubeclient.CoreV1Api()
        prerun_cmds = config_data.get("PrerunGdbCommands",[])
        config_metadata=config_data.get("Components",{})
        kube_namespace = config_metadata.get("kube_namespace","default")
        sw_name = config_metadata.get("binary_name","serviceweaver")
        selector_label = f"serviceweaver/app={sw_name}"
        pods = clientset.list_namespaced_pod(
            namespace=kube_namespace, label_selector=selector_label)
        gdbSessionConfigs: List[GdbSessionConfig] = []
        for i in pods.items:
            if i._metadata.deletion_timestamp:
                continue
            logger.debug("%s\t%s\t%s" %
                (i.status.pod_ip, i.metadata.namespace, i.metadata.name))
            remoteServerConn = KubeRemoteSeverClient(
                i.metadata.name, i.metadata.namespace)
            remoteServerConn.connect()
            output = remoteServerConn.execute_command(['ps', '-eo', "pid,comm"])
            # Use a regular expression to find the PID for 'serviceweaver1'
            match = re.search(r'(\d+)\s+{}'.format(sw_name), output)
            if match:
                pid = match.group(1)
                sessionConfig= GdbSessionConfig()
                sessionConfig.binary=remoteServerConn.execute_command(['readlink', f'/proc/{pid}/exe',])
                sessionConfig.remote_port=30001
                sessionConfig.remote_host=i.status.pod_ip
                sessionConfig.gdb_mode=GdbMode.REMOTE
                sessionConfig.remote_gdbserver=remoteServerConn
                sessionConfig.gdb_controller=ServiceWeaverkubeGdbController(i.metadata.name, i.metadata.namespace,"serviceweaver",True)
                sessionConfig.tag=i.status.pod_ip
                sessionConfig.start_mode=StartMode.ATTACH
                sessionConfig.attach_pid=int(pid)
                sessionConfig.prerun_cmds=prerun_cmds
                sessionConfig.initialize_commands.append(f"-file-exec-and-symbols /proc/{sessionConfig.attach_pid}/root{sessionConfig.binary}")
                # sessionConfig.prerun_cmds.append({"name":"add symbols","command":f"file /proc/{sessionConfig.attach_pid}/root{sessionConfig.binary}"})
                gdbSessionConfigs.append(sessionConfig)
            else:
                logger.error(i.status.pod_ip, i.metadata.name,
                    "cannot locate service weaver process:", sw_name)
        ddb_config.gdb_sessions_configs=gdbSessionConfigs

    def parse_vanillapid_config(ddb_config: DDBConfig, config_data: any):
        pids=config_data.get("Pids",[])
        for pid in pids:
            sessionConfig= GdbSessionConfig()
            sessionConfig.tag=str(pid)
            sessionConfig.attach_pid=pid
            sessionConfig.start_mode=StartMode.ATTACH
            sessionConfig.gdb_mode=GdbMode.REMOTE
            sessionConfig.prerun_cmds=config_data.get("PrerunGdbCommands",[])
            sessionConfig.gdb_controller=VanillaPIDController(pid,True)
            # sessionConfig.remote_gdbserver=LocalClient()
            # sessionConfig.initialize_commands.append(f"-file-exec-and-symbols /proc/{pid}/exe")
            ddb_config.gdb_sessions_configs.append(sessionConfig)

    @staticmethod
    def parse_config_file(config_data: any) -> DDBConfig:
        ddb_config = DDBConfig()
        if "Framework" in config_data:
            if config_data["Framework"] == "serviceweaver_kube":
                ddb_config.framework = TargetFramework.SERVICE_WEAVER_K8S
                GlobalConfig.parse_serviceweaver_kube_config(ddb_config, config_data)
            elif config_data["Framework"] == "Nu":
                ddb_config.framework = TargetFramework.NU
                GlobalConfig.parse_nu_config(ddb_config, config_data)
            elif config_data["Framework"] == "vanillapid":
                ddb_config.framework = TargetFramework.UNSPECIFIED
                GlobalConfig.parse_vanillapid_config(ddb_config, config_data)
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
                    # Set parsed config to the global scope
                    GlobalConfig.set_config(GlobalConfig.parse_config_file(config_data))
                except YAMLError as e:
                    logger.error(f"Failed to read the debugging config. Error: {e}")
                    return False
        else:
            logger.debug("Config file path is not specified...")
            return False
        return True
