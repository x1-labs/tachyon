#!/usr/bin/env bash
set -ex

[[ $(uname) = Linux ]] || exit 1
[[ $USER = root ]] || exit 1

if grep -q x1 /etc/passwd ; then
  echo "User x1 already exists"
else
  adduser x1 --gecos "" --disabled-password --quiet
  adduser x1 sudo
  adduser x1 adm
  echo "x1 ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers
  id solana

  [[ -r /solana-scratch/id_ecdsa ]] || exit 1
  [[ -r /solana-scratch/id_ecdsa.pub ]] || exit 1

  sudo -u x1 bash -c "
    echo 'PATH=\"/home/solana/.cargo/bin:$PATH\"' > /home/solana/.profile
    mkdir -p /home/solana/.ssh/
    cd /home/solana/.ssh/
    cp /solana-scratch/id_ecdsa.pub authorized_keys
    umask 377
    cp /solana-scratch/id_ecdsa id_ecdsa
    echo \"
      Host *
      BatchMode yes
      IdentityFile ~/.ssh/id_ecdsa
      StrictHostKeyChecking no
    \" > config
  "
fi
