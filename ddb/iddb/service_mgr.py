import time
from typing import Callable
import paho.mqtt.client as paho
from paho.mqtt.client import CallbackAPIVersion

from iddb.data_struct import ServiceInfo
from iddb.logging import logger
from iddb.utils import ip_int2ip_str
from iddb.startup import start_mosquitto_broker, cleanup_mosquitto_broker
from iddb.config import GlobalConfig
from iddb.const import ServiceDiscoveryConst

ON_NEW_SERVICE_CALLBACK_HANDLE = "on_new_service"
ON_NEW_SERVICE_CALLBACK = Callable[[ServiceInfo], None]

class ServiceManager:
    FIRST_RECONNECT_DELAY = 1
    RECONNECT_RATE = 2
    MAX_RECONNECT_COUNT = 12
    MAX_RECONNECT_DELAY = 60

    def __init__(self) -> None:
        broker_info = GlobalConfig.get().broker
        start_mosquitto_broker(broker_info)
        self.userdata = {
            ON_NEW_SERVICE_CALLBACK_HANDLE: self.__default_on_new_service
        }

        self.client = paho.Client(
            callback_api_version=CallbackAPIVersion.VERSION2, 
            client_id=ServiceDiscoveryConst.CLIENT_ID, 
            userdata=self.userdata, 
            protocol=paho.MQTTv5,
            transport=ServiceDiscoveryConst.BROKER_MSG_TRANSPORT
        )
        self.client.on_connect = ServiceManager.__on_connect
        self.client.on_disconnect = ServiceManager.__on_disconnect

        self.client.on_message = ServiceManager.__on_message
        self.client.message_callback_add(ServiceDiscoveryConst.T_SERVICE_DISCOVERY, ServiceManager.__on_service_discovery)

        self.client.connect(broker_info.hostname, broker_info.port)
        self.client.subscribe(ServiceDiscoveryConst.T_SERVICE_DISCOVERY)
        self.client.loop_start()

    def __del__(self) -> None:
        self.client.loop_stop()
        cleanup_mosquitto_broker()

# -----------------------------------------------
# Callbacks will be triggered by ServiceManager
# external users should set their own callbacks
# -----------------------------------------------
    def set_callback_on_new_service(self, callback: ON_NEW_SERVICE_CALLBACK):
        self.userdata[ON_NEW_SERVICE_CALLBACK_HANDLE] = callback

    def __default_on_new_service(self, service: ServiceInfo):
        logger.debug(f"Default on_new_service handle. new service discovered: {service}")

# -----------------------------------------------
# Callbacks will be triggered by 
# the internal paho MQTT client
# -----------------------------------------------
    def __on_message(client: paho.Client, userdata, message: paho.MQTTMessage):
        ''' Handling all other incoming messages excluding service discovery report message
        '''
        logger.debug(f"Received message: {message.payload.decode()}")

    def __on_service_discovery(client: paho.Client, userdata, message: paho.MQTTMessage):
        ''' Handling service discovery messages
        '''
        msg = message.payload.decode()
        logger.debug(f"Receive new service msg: {msg}")
        parts = msg.split(":")
        userdata[ON_NEW_SERVICE_CALLBACK_HANDLE](
            ServiceInfo(
                ip=ip_int2ip_str(int(parts[0])), # ip addr embedded in the message is in integer format, convert it to human-readable string
                tag=str(parts[1]), 
                pid=int(parts[2])
            )
        )
        
    def __on_connect(client: paho.Client, userdata, flags, rc, properties):
        if rc == 0:
            logger.debug("Connected to MQTT Broker!")
        else:
            logger.debug("Failed to connect, return code %d\n", rc)

    def __on_disconnect(client: paho.Client, userdata, flags, rc, properties):
        ''' auto-reconnection logic here
        '''
        logger.debug("Disconnected with result code: %s", rc)
        reconnect_count, reconnect_delay = 0, ServiceManager.FIRST_RECONNECT_DELAY
        while reconnect_count < ServiceManager.MAX_RECONNECT_COUNT:
            logger.debug("Reconnecting in %d seconds...", reconnect_delay)
            time.sleep(reconnect_delay)

            try:
                client.reconnect()
                logger.debug("Reconnected successfully!")
                return
            except Exception as err:
                logger.debug("%s. Reconnect failed. Retrying...", err)

            reconnect_delay *= ServiceManager.RECONNECT_RATE
            reconnect_delay = min(reconnect_delay, ServiceManager.MAX_RECONNECT_DELAY)
            reconnect_count += 1
        logger.debug("Reconnect failed after %s attempts. Exiting...", reconnect_count)