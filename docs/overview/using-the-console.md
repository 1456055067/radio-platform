# Using the console

The console is the web page people use day to day. You sign in through
your organisation's single sign-on. The menu only shows what your role
allows.

## Pages

| Page | What it is for |
|---|---|
| **Dashboard** | Every scanner you can see, with its current status and activity. |
| **Virtual scanner** | A live view of one scanner, laid out like its front panel. Listen and, if you have control rights, adjust it. |
| **Scanners** | The list of scanners and their details. Administrators add, change and remove scanners here. |
| **Sites** | The locations where scanners are installed. |
| **Transmissions** | A searchable history of what scanners picked up, with recordings and transcripts when those are switched on. |
| **Analytics** | Activity over time and the busiest systems and channels. |
| **Events** | Things that happened: a scanner going offline, coming back, and so on. |
| **Audit** | Who did what and when. Includes a check that the log has not been altered. |

Features an administrator has not switched on are shown as
"not available" rather than hidden or faked.

## Roles

Your organisation decides who gets which role. A typical set-up is:

| Role | Can |
|---|---|
| **Viewer** | See scanners, their status, events and transmission history. |
| **Operator** | Everything a viewer can, plus listen live and control scanners. |
| **Administrator** | Everything, including adding sites and scanners, switching on audio, recording and transcription, revoking someone else's control, and reading the audit log. |

People can also be limited to particular sites.

## Controlling a scanner

1. Open the scanner in the **virtual scanner** page.
2. Take control. If someone else has it, you will see who.
3. Make your changes. Sliders send the final value once you let go.
4. Leave the page when you are done; control is released automatically.

If your control is taken away (for example by an administrator), the page
tells you and stops sending changes.

## Things to know

- Status that has not been refreshed for a while is shown as **stale**, so
  you are never misled by an old reading.
- A scanner can only listen to one thing at a time. If it is busy on one
  channel, it is not hearing another.
- Transcripts are produced by speech-recognition software from narrow-band
  radio audio and **will contain mistakes**. Check the recording before
  relying on a transcript.
