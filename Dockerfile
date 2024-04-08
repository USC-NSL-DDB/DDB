# Use an official Python runtime as a parent image
FROM python:3.11-alpine

# Set the working directory in the container
WORKDIR /usr/src/app

RUN apk update && apk add --no-cache gdb
# Install Node.js and npm
RUN apk update && apk add --no-cache nodejs npm
# Copy the current directory contents into the container at /usr/src/app
COPY . .

# Install any needed packages specified in requirements.txt
RUN pip3 install --no-cache-dir -r py_testing/requirements.txt

# Make port 80 available to the world outside this container
EXPOSE 80

# Define environment variable
ENV NAME World

# Run app.py when the container launches
# CMD ["python", "py_testing/main.py"]
CMD ["/bin/sh"]
