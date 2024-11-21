import logging
import os
from iddb.about import DEBUG

class CustomFilter(logging.Filter):
    def filter(self, record):
        # Extract the filename without the extension
        filename = os.path.splitext(record.filename)[0]
        record.custom_filename = f'{filename}'
        return True

# Create a logger for the current module
logger = logging.getLogger("DDB")
if DEBUG:
    logger.setLevel(logging.DEBUG)  # Set the desired logging level
else:
    logger.setLevel(logging.INFO)

# Create a console handler
console_handler = logging.StreamHandler()
if DEBUG:
    console_handler.setLevel(logging.DEBUG)  # Set the desired logging level for the handler
else:
    console_handler.setLevel(logging.INFO)

# Create a file handler
os.makedirs("/tmp/ddb", exist_ok=True)
file_handler = logging.FileHandler('/tmp/ddb/ddb.log')
if DEBUG:
    file_handler.setLevel(logging.DEBUG)  # Set the desired logging level for the handler
else:
    file_handler.setLevel(logging.INFO)

# Create a formatter and set it for the handler
formatter = logging.Formatter('%(asctime)s | %(name)s.%(custom_filename)s <%(levelname)s> | %(message)s')
console_handler.setFormatter(formatter)
file_handler.setFormatter(formatter)

# Add the handler to the logger
logger.addHandler(console_handler)
logger.addHandler(file_handler)

# Add the custom filter to the logger
logger.addFilter(CustomFilter())

# Suppress logs from other modules by setting a higher log level for the root logger
# logging.getLogger().setLevel(logging.WARNING)

# Function to temporarily disable logging
def disable_logging():
    logger.setLevel(logging.CRITICAL)

# Function to enable logging back to the previous level
def enable_logging():
    logger.setLevel(logging.DEBUG)
