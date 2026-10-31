# Fleet runtime paths

Local flake contexts use the canonical checkout path from ContextIdentity.
Ansible commands run in that checkout's `ansible/` directory when it contains
`ansible.cfg`, otherwise in the checkout root. Starting a leader or follower
from another directory does not select another inventory.

Reboot runs `nix run <canonical-checkout>#ans-reboot -- -l <target> ...`.
The fleet flake must expose `apps.<system>.ans-reboot`. The app must supply
the controller-side Python environment (including kubernetes and hvac) used
by delegated drain and Ready steps. A missing app fails with Nix's error
naming the requested `<checkout>#ans-reboot` installable; there is no fallback
to the leader's PATH or a raw playbook.
