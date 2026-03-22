# Vale
Salsa is currently running on vale.oso.chalmers.se

## Looking at logs
Use journalctl, e.g.
```bash
sudo journalctl -u salsa -f -p warning
```

## Restarting the service
After changing configuration you need to restart the service, this is done via
```bash
sudo systemctl restart salsa
```
