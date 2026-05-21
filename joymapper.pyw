import sys
import ctypes
from ctypes import wintypes
import os
import json
import time
import winreg

# --- 1. NATIVE BOOTLOADER & DEPENDENCIES ---
def ensure_dependencies():
    missing = []
    dependencies = [("PyQt6", "PyQt6"), ("XInput", "XInput-Python"), ("keyboard", "keyboard")]
    for module_name, pip_name in dependencies:
        try: __import__(module_name)
        except ImportError: missing.append(pip_name)
            
    if missing:
        cmd = "py -m pip install " + " ".join(missing)
        msg = f"Missing libraries. Open Command Prompt and run:\n\n{cmd}"
        ctypes.windll.user32.MessageBoxW(0, msg, "Setup Required", 0x10)
        sys.exit(1)

ensure_dependencies()

import keyboard
import XInput
from PyQt6.QtWidgets import (QApplication, QMainWindow, QTableWidget, QTableWidgetItem, 
                             QVBoxLayout, QHBoxLayout, QWidget, QHeaderView, QInputDialog,
                             QTabWidget, QCheckBox, QComboBox, QLabel, QPushButton, 
                             QSystemTrayIcon, QMenu, QFormLayout)
from PyQt6.QtCore import QThread, pyqtSignal, Qt, QSettings
from PyQt6.QtGui import QColor, QIcon, QAction

# --- 2. WINDOWS 11 MICA & SHELL32 API ---
def apply_windows_11_mica(hwnd):
    """Applies Windows 11 Mica backdrop effect to the window."""
    if sys.platform != "win32" or sys.getwindowsversion().build < 22000:
        return
    try:
        DWMWA_USE_IMMERSIVE_DARK_MODE = 20
        DWMWA_SYSTEMBACKDROP_TYPE = 38
        ctypes.windll.dwmapi.DwmSetWindowAttribute(
            hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, ctypes.byref(ctypes.c_int(1)), 4)
        ctypes.windll.dwmapi.DwmSetWindowAttribute(
            hwnd, DWMWA_SYSTEMBACKDROP_TYPE, ctypes.byref(ctypes.c_int(2)), 4) # 2 = Mica, 3 = Acrylic
    except Exception:
        pass

def get_shell32_icon():
    """Extracts a hardware/gamepad icon directly from shell32.dll using pure Windows API."""
    shell32 = ctypes.windll.shell32
    user32 = ctypes.windll.user32
    hicon = ctypes.c_void_p()
    # Icon index 176 is often a hardware/device icon in shell32.dll
    shell32.ExtractIconExW("shell32.dll", 176, ctypes.byref(hicon), None, 1)
    return hicon

# --- 3. CONTROLLER THREAD (FULL INPUTS) ---
class ControllerThread(QThread):
    button_pressed = pyqtSignal(str)
    button_released = pyqtSignal(str)
    connected_status = pyqtSignal(bool, str) # status, device_info

    def __init__(self):
        super().__init__()
        self.is_running = True
        self.is_paused = False
        self.target_controller = "Auto" # "Auto", 0, 1, 2, 3
        self.analog_state = {}
        self.deadzone = 0.5

    def get_active_controller(self):
        connected = XInput.get_connected()
        if self.target_controller != "Auto":
            idx = int(self.target_controller)
            return idx if connected[idx] else -1
        for i in range(4):
            if connected[i]: return i
        return -1

    def handle_analog_to_digital(self, name, value):
        is_pressed = value > self.deadzone
        was_pressed = self.analog_state.get(name, False)
        if is_pressed and not was_pressed:
            self.button_pressed.emit(name)
            self.analog_state[name] = True
        elif not is_pressed and was_pressed:
            self.button_released.emit(name)
            self.analog_state[name] = False

    def run(self):
        was_connected = False
        while self.is_running:
            idx = self.get_active_controller()
            is_connected = idx != -1

            if is_connected != was_connected:
                info = f"Controller {idx}" if is_connected else "None"
                self.connected_status.emit(is_connected, info)
                was_connected = is_connected

            if is_connected and not self.is_paused:
                events = XInput.get_events()
                for event in events:
                    if event.user_index != idx: continue

                    # Digital Buttons
                    if event.type == XInput.EVENT_BUTTON_PRESSED:
                        self.button_pressed.emit(event.button)
                    elif event.type == XInput.EVENT_BUTTON_RELEASED:
                        self.button_released.emit(event.button)
                    
                    # Triggers (Analog to Digital)
                    elif event.type == XInput.EVENT_TRIGGER_MOVED:
                        if event.trigger == XInput.LEFT:
                            self.handle_analog_to_digital("LEFT_TRIGGER", event.value)
                        elif event.trigger == XInput.RIGHT:
                            self.handle_analog_to_digital("RIGHT_TRIGGER", event.value)
                    
                    # Sticks (Analog to Digital)
                    elif event.type == XInput.EVENT_STICK_MOVED:
                        prefix = "LS_" if event.stick == XInput.LEFT else "RS_"
                        self.handle_analog_to_digital(prefix + "UP", event.y)
                        self.handle_analog_to_digital(prefix + "DOWN", -event.y)
                        self.handle_analog_to_digital(prefix + "RIGHT", event.x)
                        self.handle_analog_to_digital(prefix + "LEFT", -event.x)
            
            time.sleep(0.01)

    def stop(self):
        self.is_running = False
        self.wait()

# --- 4. MAIN APPLICATION ---
class JoyToKeyClone(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Pro Controller Mapper")
        self.resize(600, 500)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground) # Needed for Mica
        
        self.app_settings = QSettings("MyCompany", "JoyMapperPro")
        self.profile_file = "joymapper_profiles.json"
        
        # Every single XInput button/axis
        self.all_inputs = [
            "A", "B", "X", "Y", "DPAD_UP", "DPAD_DOWN", "DPAD_LEFT", "DPAD_RIGHT",
            "LEFT_SHOULDER", "RIGHT_SHOULDER", "LEFT_THUMB", "RIGHT_THUMB", "START", "BACK",
            "LEFT_TRIGGER", "RIGHT_TRIGGER", 
            "LS_UP", "LS_DOWN", "LS_LEFT", "LS_RIGHT", 
            "RS_UP", "RS_DOWN", "RS_LEFT", "RS_RIGHT"
        ]
        
        self.profiles = {"Default": {inp: "" for inp in self.all_inputs}}
        self.current_profile = "Default"
        self.row_map = {} 
        self.load_profiles()

        self.init_ui()
        self.init_tray()
        
        self.thread = ControllerThread()
        self.thread.button_pressed.connect(self.on_button_pressed)
        self.thread.button_released.connect(self.on_button_released)
        self.thread.connected_status.connect(self.on_connection_change)
        
        # Load settings
        self.chk_tray.setChecked(self.app_settings.value("close_to_tray", True, type=bool))
        self.chk_startup.setChecked(self.check_startup_registry())
        self.thread.target_controller = self.app_settings.value("controller", "Auto")
        self.combo_controller.setCurrentText(self.thread.target_controller)
        
        self.thread.start()

    def showEvent(self, event):
        super().showEvent(event)
        apply_windows_11_mica(int(self.winId()))
        
        # Apply extracted Shell32 Icon via direct Windows message
        hicon = get_shell32_icon()
        if hicon:
            WM_SETICON = 0x0080
            ctypes.windll.user32.SendMessageW(int(self.winId()), WM_SETICON, 0, hicon)
            ctypes.windll.user32.SendMessageW(int(self.winId()), WM_SETICON, 1, hicon)

    def init_ui(self):
        tabs = QTabWidget()
        self.setCentralWidget(tabs)

        # -- TAB 1: MAPPINGS --
        tab_map = QWidget()
        map_layout = QVBoxLayout(tab_map)
        
        # Profile selector
        prof_layout = QHBoxLayout()
        self.combo_profiles = QComboBox()
        self.combo_profiles.addItems(self.profiles.keys())
        self.combo_profiles.currentTextChanged.connect(self.switch_profile)
        btn_new_prof = QPushButton("New Profile")
        btn_new_prof.clicked.connect(self.new_profile)
        btn_del_prof = QPushButton("Delete")
        btn_del_prof.clicked.connect(self.delete_profile)
        
        prof_layout.addWidget(QLabel("Profile:"))
        prof_layout.addWidget(self.combo_profiles)
        prof_layout.addWidget(btn_new_prof)
        prof_layout.addWidget(btn_del_prof)
        map_layout.addLayout(prof_layout)

        # Table
        self.table = QTableWidget()
        self.table.setColumnCount(2)
        self.table.setHorizontalHeaderLabels(["Controller Input", "Mapped Key(s)"])
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        self.table.doubleClicked.connect(self.edit_mapping)
        self.table.setStyleSheet("background-color: rgba(30, 30, 30, 150); color: white;") # Dark transparent
        
        map_layout.addWidget(self.table)
        self.refresh_table()
        tabs.addTab(tab_map, "Mappings")

        # -- TAB 2: SETTINGS --
        tab_settings = QWidget()
        set_layout = QFormLayout(tab_settings)
        
        self.chk_tray = QCheckBox("Close to System Tray")
        self.chk_tray.stateChanged.connect(lambda: self.app_settings.setValue("close_to_tray", self.chk_tray.isChecked()))
        
        self.chk_startup = QCheckBox("Run automatically on Windows Startup")
        self.chk_startup.stateChanged.connect(self.toggle_startup)
        
        self.combo_controller = QComboBox()
        self.combo_controller.addItems(["Auto", "0", "1", "2", "3"])
        self.combo_controller.currentTextChanged.connect(self.change_controller)
        
        self.lbl_device_info = QLabel("Searching...")
        self.lbl_device_info.setStyleSheet("color: #00ff00; font-weight: bold;")
        
        set_layout.addRow("Startup:", self.chk_startup)
        set_layout.addRow("Behavior:", self.chk_tray)
        set_layout.addRow("Target Device:", self.combo_controller)
        set_layout.addRow("Status:", self.lbl_device_info)
        
        tabs.addTab(tab_settings, "Settings")

    def init_tray(self):
        self.tray = QSystemTrayIcon(self)
        # Using a default system icon for tray to avoid memory leaks with hicon
        self.tray.setIcon(self.style().standardIcon(self.style().StandardPixmap.SP_ComputerIcon))
        
        menu = QMenu()
        act_show = QAction("Open UI", self)
        act_show.triggered.connect(self.showNormal)
        
        self.act_pause = QAction("Pause Mappings", self)
        self.act_pause.setCheckable(True)
        self.act_pause.triggered.connect(self.toggle_pause)
        
        act_exit = QAction("Exit", self)
        act_exit.triggered.connect(self.force_exit)
        
        menu.addAction(act_show)
        menu.addAction(self.act_pause)
        menu.addSeparator()
        menu.addAction(act_exit)
        
        self.tray.setContextMenu(menu)
        self.tray.activated.connect(self.tray_activated)
        self.tray.show()

    # --- LOGIC & EVENTS ---
    def load_profiles(self):
        if os.path.exists(self.profile_file):
            try:
                with open(self.profile_file, "r") as f:
                    self.profiles = json.load(f)
            except Exception: pass
            
    def save_profiles(self):
        with open(self.profile_file, "w") as f:
            json.dump(self.profiles, f, indent=4)

    def refresh_table(self):
        mapping = self.profiles[self.current_profile]
        self.table.setRowCount(len(self.all_inputs))
        for row, btn in enumerate(self.all_inputs):
            self.table.setItem(row, 0, QTableWidgetItem(btn))
            self.table.setItem(row, 1, QTableWidgetItem(mapping.get(btn, "")))
            self.row_map[btn] = row

    def switch_profile(self, name):
        if name in self.profiles:
            self.current_profile = name
            self.refresh_table()

    def new_profile(self):
        name, ok = QInputDialog.getText(self, "New Profile", "Profile Name:")
        if ok and name and name not in self.profiles:
            self.profiles[name] = {inp: "" for inp in self.all_inputs}
            self.combo_profiles.addItem(name)
            self.combo_profiles.setCurrentText(name)
            self.save_profiles()

    def delete_profile(self):
        if len(self.profiles) > 1:
            del self.profiles[self.current_profile]
            self.combo_profiles.removeItem(self.combo_profiles.currentIndex())
            self.save_profiles()

    def toggle_pause(self, state):
        self.thread.is_paused = state
        self.tray.showMessage("Controller Mapper", "Mappings Paused" if state else "Mappings Active")

    def change_controller(self, text):
        self.thread.target_controller = text
        self.app_settings.setValue("controller", text)

    def check_startup_registry(self):
        try:
            key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", 0, winreg.KEY_READ)
            winreg.QueryValueEx(key, "JoyMapperPro")
            winreg.CloseKey(key)
            return True
        except FileNotFoundError: return False

    def toggle_startup(self):
        try:
            key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", 0, winreg.KEY_SET_VALUE)
            if self.chk_startup.isChecked():
                path = f'"{sys.executable}" "{os.path.abspath(__file__)}"'
                winreg.SetValueEx(key, "JoyMapperPro", 0, winreg.REG_SZ, path)
            else:
                winreg.DeleteValue(key, "JoyMapperPro")
            winreg.CloseKey(key)
        except Exception as e:
            print(f"Registry Error: {e}")

    # --- INPUT EXECUTION ---
    def on_button_pressed(self, button):
        if button in self.row_map:
            row = self.row_map[button]
            for col in range(2): 
                self.table.item(row, col).setBackground(QColor(50, 150, 255, 150)) # Highlight Blue
            
            mapped_key = self.profiles[self.current_profile].get(button, "")
            if mapped_key:
                try: keyboard.press(mapped_key)
                except Exception: pass

    def on_button_released(self, button):
        if button in self.row_map:
            row = self.row_map[button]
            for col in range(2): 
                self.table.item(row, col).setBackground(QColor(Qt.GlobalColor.transparent))
            
            mapped_key = self.profiles[self.current_profile].get(button, "")
            if mapped_key:
                try: keyboard.release(mapped_key)
                except Exception: pass

    def on_connection_change(self, is_connected, info):
        status = "🟢 Connected" if is_connected else "🔴 Disconnected"
        self.lbl_device_info.setText(f"{status} ({info})")
        self.setWindowTitle(f"Pro Controller Mapper - [{info}]")

    def edit_mapping(self, index):
        row = index.row()
        button_item = self.table.item(row, 0).text()
        current_key = self.table.item(row, 1).text()

        new_key, ok = QInputDialog.getText(self, "Edit Mapping", f"Bind key/macro for '{button_item}':", text=current_key)
        if ok and new_key is not None:
            new_key = new_key.strip().lower()
            self.profiles[self.current_profile][button_item] = new_key
            self.table.item(row, 1).setText(new_key)
            self.save_profiles()

    def tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.DoubleClick:
            self.showNormal()
            self.activateWindow()

    def closeEvent(self, event):
        if self.chk_tray.isChecked():
            event.ignore()
            self.hide()
            self.tray.showMessage("Controller Mapper", "Running in background...")
        else:
            self.force_exit()

    def force_exit(self):
        self.thread.stop()
        self.tray.hide()
        QApplication.quit()

if __name__ == "__main__":
    # Ensure High DPI Scaling looks good
    os.environ["QT_ENABLE_HIGHDPI_SCALING"] = "1"
    QApplication.setHighDpiScaleFactorRoundingPolicy(Qt.HighDpiScaleFactorRoundingPolicy.PassThrough)
    
    app = QApplication(sys.argv)
    
    # Modern dark theme settings
    app.setStyle("Fusion")
    palette = app.palette()
    palette.setColor(palette.ColorRole.Window, QColor(20, 20, 20, 200))
    palette.setColor(palette.ColorRole.WindowText, Qt.GlobalColor.white)
    palette.setColor(palette.ColorRole.Base, QColor(30, 30, 30, 150))
    palette.setColor(palette.ColorRole.Text, Qt.GlobalColor.white)
    app.setPalette(palette)
    
    window = JoyToKeyClone()
    window.show()
    
    sys.exit(app.exec())