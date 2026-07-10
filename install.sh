#!/usr/bin/env bash
###########################
# linux-three-finger-drag #
#   Installation Script   #
###########################

# echo multi-line string (standard echo doesn't work well with tabs)
# this also makes sure the printed lines wrap on spaces, not in the
# middle of words
echo-mls() {
    echo -e "$1" | fold -s -w $(( $(tput cols) - 5 ))
}

# don't run if not root
if [[ $(whoami) != "root" ]]; then
    echo-mls "\n\e[0;31mFatal\e[0m: Root privileges are needed to install this program \
        and configure the relevant settings (including kernel modules to load at boot)." 
    exit 1
fi

# 1. (There are no C library prerequisites: the program talks to evdev
#    and uinput directly. Only a Rust toolchain is needed to build.)
echo -ne "Verifying prerequisites...                      "

# verify CWD is the repo folder
if [[ ${PWD##*/} != "linux-3-finger-drag" ]]; then
    echo -e "[\e[0;31m FAIL \e[0m]"
    echo-mls "\n\e[0;31mFatal\e[0m: This script needs to be run from the repo directory \
        (linux-3-finger-drag) to run properly. Either return to that directory, \
        or, if you're already there, change the name back to linux-3-finger-drag."
    exit 1
fi

echo -e "[\e[0;32m DONE \e[0m]"

# (2. repo already cloned, presumably)


# 3. Update permissions
echo -n "Updating permissions...                         "
## Update udev rules
mkdir -p /etc/udev/rules.d   # make if not already extant
cp ./60-uinput.rules /etc/udev/rules.d/

## Add user to "input" group to read /dev/input devices directly
gpasswd --add "$SUDO_USER" input > /dev/null

## Automatically load uinput kernel module
## Not necessary on Ubuntu-based distros,
## But essential on Arch (and probably more minimal distros too), 
## and does no harm on other distros
echo "uinput" > /etc/modules-load.d/uinput.conf
modprobe uinput

echo -e "[\e[0;32m DONE \e[0m]"


# 4. Build with Cargo
echo "Compiling..."
echo

## this needs to be done as the user, or else is messes up the permissions.
## Cargo should never really be run as root anyway.
REPO_DIR=$PWD
su -l "$SUDO_USER" -c "cd '$REPO_DIR'; cargo build --release"
CARGO_EXIT_CODE=$?

if [ $CARGO_EXIT_CODE -ne 0 ]; then
    echo-mls "\n\e[0;31mBuild failed.\e[0m Check the Cargo output above; a plain \
        'cargo build --release' in this directory should reproduce it."
    exit $CARGO_EXIT_CODE
fi
echo


# 5. Install to /usr/bin
# If you're getting permission issues (after a reboot), try setting the 
# setuid bit, so it will execute as root, by running
#     
#     chmod u+s /usr/bin/linux-3-finger-drag 
#
# You can also change its owner to root by a simple:
#
#     chown root:root /usr/bin/linux-3-finger-drag
# 
# Neither of these are preferred security-wise (unless you trust my program), 
# but it should solve the issue.
# Determine the install location. On immutable / atomic distros (Fedora
# Silverblue / Kinoite / Aurora / Bazzite, openSUSE MicroOS, etc.) /usr is a
# read-only mount, so writing to /usr/bin fails. In that case we fall back to
# the user's ~/.local/bin, which is writable and already on PATH. The SystemD
# unit's ExecStart is generated to match the chosen path (see below).
USER_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
if touch /usr/bin/.3fd-write-test 2>/dev/null; then
    rm -f /usr/bin/.3fd-write-test
    INSTALL_DIR=/usr/bin
else
    INSTALL_DIR="$USER_HOME/.local/bin"
fi
BIN_PATH="$INSTALL_DIR/linux-3-finger-drag"

echo -n "Installing binary to $INSTALL_DIR... "

# with the user added to the 'input' group, root does not need to own the
# executable, so keep it owned by the user (principle of least privilege, and
# required when installing into the user's home directory).
if [[ $INSTALL_DIR == /usr/bin ]]; then
    ERR_MSG=$(cp --preserve=ownership ./target/release/linux-3-finger-drag /usr/bin/ 2>&1)
else
    ERR_MSG=$(su "$SUDO_USER" -c "mkdir -p '$INSTALL_DIR' && cp '$REPO_DIR/target/release/linux-3-finger-drag' '$INSTALL_DIR'/" 2>&1)
fi
if [[ -n $ERR_MSG ]]; then
    echo -e "[\e[0;33m WARN \e[0m]"
    echo -e "\e[0;33mWarning\e[0m: Could not install binary to $INSTALL_DIR:"
    echo "    $ERR_MSG"
    echo "    This may cause issues with the SystemD service."
else
    echo -e "[\e[0;32m DONE \e[0m]"
fi


# Set up config file
# Has to be done as non-root user, so the file is accessible to the user
echo -n "Installing config file...                       "
su "$SUDO_USER" -c '\
    mkdir -p ~/.config/linux-3-finger-drag; \
    cp 3fd-config.json ~/.config/linux-3-finger-drag '
echo -e "[\e[0;32m DONE \e[0m]"


# (7a. KDE Autostart needs to be configured through GUI)

# 7b. Installing SystemD service
# If using SystemD as the init system
echo -n "Installing/enabling SystemD user unit...        "
if ps -p 1 | grep -q systemd; then

    # define user-level service, made as the non-root user.
    # - ExecStart is rewritten to the path the binary was actually installed to
    #   (matters when we fell back to ~/.local/bin on an immutable distro).
    # - XDG_RUNTIME_DIR must be set explicitly: `systemctl --user` invoked
    #   through su has no session bus in its environment, which otherwise fails
    #   with "Failed to connect to user scope bus via local transport".
    USER_UID=$(id -u "$SUDO_USER")
    su "$SUDO_USER" -c "\
        mkdir -p \$HOME/.config/systemd/user; \
        sed 's|^ExecStart=.*|ExecStart=$BIN_PATH|' '$REPO_DIR/three-finger-drag.service' > \$HOME/.config/systemd/user/three-finger-drag.service; \
        export XDG_RUNTIME_DIR=/run/user/$USER_UID; \
        systemctl --user daemon-reload; \
        systemctl --user enable --now three-finger-drag.service"
    echo -e "[\e[0;32m DONE \e[0m]"

else
    echo -e "[\e[0;33m WARN \e[0m]"
    echo -e "\n\e[0;33mWarning: Your system doesn't use SystemD.\e[0m"
    echo-mls "Currently, only SystemD installation is automated by this install script, \
        so you'll have to use create and enable the service for your init system."
    echo
    echo "You may also have to ensure that the uinput kernel module loads on boot."
    echo "The config has been added in /etc/modules-load.d/uinput.conf."
    echo
    echo-mls "(If I get enough requests for it I'll adapt this install script for other inits, \
        probably starting with OpenRC. Also, feel free to submit a pull request for this.)"
fi

## 6. Reboot
echo
echo "This installation requires a reboot to complete (for the group modification)."
echo
echo -n "Would you like to reboot now? (y/n, default y) "
read -r answer

case "$answer" in 
    y | "")
        echo "Okay! Rebooting now..."
        reboot
    ;;
    n)
        echo -e "\n\e[0;33mWarning\e[0m: Not rebooting. The program will start working after the next boot."
    ;;
    *)
        echo-mls "\n\e[0;33mWarning\e[0m: Response not recognized, not rebooting. The program will start \
            working after the next boot."
esac


echo
echo -e "\e[0;32mInstall complete!\e[0m (pending reboot)"
echo
