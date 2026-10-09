#!/bin/sh
# 实验台自签证书（仅本实验台；产品形态用 token 派生公钥钉定）
set -e
cd "$(dirname "$0")"
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 3650 -nodes -subj "/CN=localhost" >/dev/null 2>&1
openssl x509 -in cert.pem -outform DER -out cert.der
openssl pkcs8 -topk8 -nocrypt -in key.pem -outform DER -out key.der
echo "生成: cert.der ($(wc -c < cert.der)B) key.der ($(wc -c < key.der)B)"
