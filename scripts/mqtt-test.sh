#!/bin/bash
# Simple MQTT test setup for Rubicson sensor development
# Usage: ./mqtt-test.sh [broker|sub|pub]

set -e

BROKER_PORT=1883
TOPIC="sensors/rubicson/#"

# Create temp config to listen on all interfaces (not just localhost)
CONF_FILE=$(mktemp)
cat > "$CONF_FILE" << EOF
listener $BROKER_PORT 0.0.0.0
allow_anonymous true
max_keepalive 300
EOF
trap "rm -f $CONF_FILE" EXIT

case "${1:-all}" in
    broker)
        echo "Starting Mosquitto broker on port $BROKER_PORT (all interfaces)..."
        mosquitto -c "$CONF_FILE" -v
        ;;
    sub)
        echo "Subscribing to $TOPIC..."
        mosquitto_sub -h localhost -p $BROKER_PORT -t "$TOPIC" -v -F '%I %t %p'
        ;;
    pub)
        # Test publish - useful for verifying subscriber works
        echo "Publishing test message..."
        mosquitto_pub -h localhost -p $BROKER_PORT \
            -t "sensors/rubicson/1234/temperature" \
            -m "id=1234,ch=1,temp=22.5,batt=ok"
        echo "Done"
        ;;
    all)
        echo "Starting broker in background and subscriber in foreground..."
        echo "Press Ctrl+C to stop"
        echo ""
        
        # Start broker in background (listening on all interfaces)
        mosquitto -c "$CONF_FILE" &
        BROKER_PID=$!
        
        # Cleanup on exit
        trap "kill $BROKER_PID 2>/dev/null; rm -f $CONF_FILE; echo 'Stopped'" EXIT
        
        sleep 0.5
        echo "Broker running (PID $BROKER_PID)"
        echo "Listening on $TOPIC..."
        echo "---"
        
        # Subscribe in foreground
        mosquitto_sub -h localhost -p $BROKER_PORT -t "$TOPIC" -v -F '%I %t %p'
        ;;
    *)
        echo "Usage: $0 [broker|sub|pub|all]"
        echo "  broker - Start broker only"
        echo "  sub    - Start subscriber only (broker must be running)"
        echo "  pub    - Publish a test message"
        echo "  all    - Start broker + subscriber (default)"
        exit 1
        ;;
esac
