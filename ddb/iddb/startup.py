import pkg_resources
import time
import os
import shutil
import subprocess

from iddb.const import ServiceDiscoveryConst

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

    broker_config = pkg_resources.resource_filename('iddb', 'conf/mosquitto.conf')
    destination_file = "/tmp/ddb/mosquitto/mosquitto.conf"
    shutil.copy(broker_config, destination_file)

# prepare the folder at the startup (import this file at very beginning of this entry point)
folder_struct_setup() 

from iddb.logging import logger
from iddb.data_struct import BrokerInfo

def start_mosquitto_broker(broker: BrokerInfo):
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
            subprocess.run(["sudo", "pkill", "-9", "mosquitto"])
        else:
            subprocess.run(["pkill", "-9", "mosquitto"])
        logger.debug("Mosquitto broker terminated successfully!")
    except Exception as e:
        logger.error(f"Failed to terminate Mosquitto broker: {e}")
