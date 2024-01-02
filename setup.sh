#!/bin/bash

# Install dependencies
bash ./dep.sh

source config.sh

set -x
git config --global user.name "$GIT_USER_NAME"
git config --global user.email "$GIT_USER_EMAIL"
