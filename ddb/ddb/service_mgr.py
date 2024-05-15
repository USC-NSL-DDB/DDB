import logging
import time
import paho.mqtt.client as paho
from paho.mqtt.client import CallbackAPIVersion

BROKER_ADDR = "10.10.2.1"
BROKER_PORT = 10101
BROKER_MSG_TRANSPORT = "tcp"
T_SERVICE_DISCOVERY = "service_discovery/report"
CLIENT_ID = "service_manager"

class ServiceManager:
    FIRST_RECONNECT_DELAY = 1
    RECONNECT_RATE = 2
    MAX_RECONNECT_COUNT = 12
    MAX_RECONNECT_DELAY = 60

    def __init__(self) -> None:
        self.client = paho.Client(
            callback_api_version=CallbackAPIVersion.VERSION2, 
            client_id=CLIENT_ID, 
            userdata=None, 
            protocol=paho.MQTTv5,
            transport=BROKER_MSG_TRANSPORT
        )
        self.client.on_connect = ServiceManager.__on_connect
        self.client.on_disconnect = ServiceManager.__on_disconnect

        self.client.on_message = ServiceManager.__on_message
        self.client.message_callback_add(T_SERVICE_DISCOVERY, ServiceManager.__on_service_discovery)

        self.client.connect(BROKER_ADDR, BROKER_PORT)
        self.client.subscribe(T_SERVICE_DISCOVERY)
        self.client.loop_start()

    def __deinit__(self) -> None:
        self.client.loop_stop()

    def __on_message(client: paho.Client, userdata, message: paho.MQTTMessage):
        ''' Handling all other incoming messages excluding service discovery report message
        '''
        logging.info(f"Received message: {message.payload.decode()}")
        print(f"Received message: {message.payload.decode()}")

    def __on_service_discovery(client: paho.Client, userdata, message: paho.MQTTMessage):
        ''' Handling service discovery messages
        '''
        logging.info(f"Service discovery message: {message.payload.decode()}")
        print(f"Service discovery message: {message.payload.decode()}")

    def __on_connect(client: paho.Client, userdata, flags, rc, properties):
        if rc == 0:
            print("Connected to MQTT Broker!")
        else:
            print("Failed to connect, return code %d\n", rc)

    def __on_disconnect(client: paho.Client, userdata, flags, rc, properties):
        ''' auto-reconnection logic here
        '''
        logging.info("Disconnected with result code: %s", rc)
        # print("Disconnected with result code: %s", rc)
        reconnect_count, reconnect_delay = 0, ServiceManager.FIRST_RECONNECT_DELAY
        while reconnect_count < ServiceManager.MAX_RECONNECT_COUNT:
            logging.info("Reconnecting in %d seconds...", reconnect_delay)
            time.sleep(reconnect_delay)

            try:
                client.reconnect()
                logging.info("Reconnected successfully!")
                return
            except Exception as err:
                logging.error("%s. Reconnect failed. Retrying...", err)

            reconnect_delay *= ServiceManager.RECONNECT_RATE
            reconnect_delay = min(reconnect_delay, ServiceManager.MAX_RECONNECT_DELAY)
            reconnect_count += 1
        logging.info("Reconnect failed after %s attempts. Exiting...", reconnect_count)