# Privacy Policy

Dude is built around a local-first philosophy. Your data belongs to you, stays on your device, and is never shared, monetized, or harvested.

---

## Summary of Guarantees

- **No Telemetry**: Dude contains zero analytics trackers, telemetry libraries, diagnostic pings, or background usage reporting.
- **No User Accounts**: You do not need to register, provide an email address, or log into any third-party account to use the application.
- **Direct AI Communication**: The app talks directly to Google's official Gemini API using your own API key. No intermediate relay servers exist.
- **In-Memory Agent Tracking**: Hook data from coding agents is parsed locally and kept in volatile RAM only.
- **Local Control Center**: Hardware adjustments operate exclusively through native Windows APIs on your machine.

---

## Detailed Data Practices

### 1. Gemini AI & API Key Storage

- **API Key Security**: When you enter your Google Gemini API key during onboarding or in Settings, it is saved exclusively into the native **Windows Credential Manager** via the Windows keyring system. The key is never saved as plain text in configuration files, never committed to disk unencrypted, and never sent to any server other than Google's official API endpoint.
- **Prompt and Chat Data**: When you interact with the Chat tab, your messages, optional system persona, and conversation context are sent straight over an encrypted HTTPS connection to Google's generative language servers (`generativelanguage.googleapis.com`). Your conversations are processed under Google's standard API terms according to your Google AI Studio account settings. Dude itself stores conversation history solely within the local application store on your computer.

### 2. Agents Hooks & Live View

- When you enable Antigravity agent integration, Dude installs a lightweight local hook binary (`dude-hook.exe`).
- As the AI agent works in your development environment, the hook passes event notifications (such as tool names like `read`, `edit`, or `bash`, targeted file paths, command strings, and short code snippets) across a local Windows Named Pipe (`\\.\pipe\dude-agent-events`).
- This information is processed locally by Dude to update the visual steps on the mascot's Agents tab.
- **Event data is held strictly in temporary memory (RAM) while the application is active.** Event contents are not written to persistent logs or transmitted across the network.

### 3. Laptop Control Center

The Control Center tab interacts solely with local Windows system components:
- **Audio & Mute**: Managed through Windows Core Audio endpoints (`IAudioEndpointVolume`).
- **Screen Brightness**: Queried and set via the Windows Management Instrumentation (WMI CIM) display interface.
- **Media Controller**: Connects to Windows System Media Transport Controls to display track title, artist, album art, and playback states for active media players running on your system.
- **Radios & Connectivity**: Wi-Fi, Bluetooth, Airplane Mode, and Mobile Hotspot are toggled using native Windows `Windows.Devices.Radios` and `Windows.Networking.Connectivity` APIs.
- **Battery**: Reads local power status through the Windows Win32 Power APIs.

None of these system metrics or status queries ever leave your machine.

---

## Network Activity

The only network communication performed by Dude is:
1. Outbound HTTPS requests to Google's official Gemini API endpoint (`https://generativelanguage.googleapis.com/*`) when testing your API key or submitting a chat prompt.
2. Web requests initiated when you explicitly click an external link (such as opening Google AI Studio or project documentation), which launches your system's default web browser.

---

## Contact & Questions

If you have questions about Dude's privacy commitments, feel free to open a discussion or issue in the GitHub repository.
