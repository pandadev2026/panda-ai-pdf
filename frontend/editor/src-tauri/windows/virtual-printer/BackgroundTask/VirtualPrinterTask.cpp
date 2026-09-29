// PHASE 1 SCAFFOLD — UNBUILT, UNTESTED. See ../../README.md.
//
// Translated (not compiled or run) from Microsoft's C# "Simple Software
// Endpoint sample for PDF and XPS printers" in the PSA v4 design guide:
//   https://learn.microsoft.com/en-us/windows-hardware/drivers/devapps/print-support-app-v4-design-guide
//
// Behavior: when a print job lands on the "Stirling PDF" queue, convert the
// input from OXPS to PDF using Windows' built-in converter and write it to
// a hardcoded temp folder. That's it for Phase 1 — deliberately NOT wired
// to launch Stirling-PDF.exe yet (see TODO(phase-2) below). Confirms the OS
// mechanics work before any app integration.

#include "pch.h"
#include "VirtualPrinterTask.h"
#if __has_include("Tasks.VirtualPrinterTask.g.cpp")
#include "Tasks.VirtualPrinterTask.g.cpp"
#endif

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Storage.h>
#include <winrt/Windows.Storage.Streams.h>

using namespace winrt;
using namespace winrt::Windows::ApplicationModel::Background;
using namespace winrt::Windows::Devices::Printers;
using namespace winrt::Windows::Graphics::Printing::Workflow;
using namespace winrt::Windows::Storage;
using namespace winrt::Windows::Storage::Streams;

namespace winrt::Tasks::implementation
{
    // Everything this virtual printer produces lands here in Phase 1.
    // TODO(phase-2): replace with a real Stirling-PDF-owned output
    // directory (see utils/paths.rs on the Rust side for the existing
    // app-data-dir convention) and, once the PDF is written, launch
    // "Stirling-PDF.exe <path>" so it flows through the existing
    // parse_launch_files / single-instance forwarding in lib.rs instead of
    // sitting on disk.
    static constexpr wchar_t kPhase1OutputDir[] = L"C:\\ProgramData\\StirlingPDF\\VirtualPrinterOutput";

    void VirtualPrinterTask::Run(IBackgroundTaskInstance const& taskInstance)
    {
        auto details = taskInstance.TriggerDetails().as<PrintWorkflowVirtualPrinterTriggerDetails>();
        m_taskDeferral = taskInstance.GetDeferral();

        PrintWorkflowVirtualPrinterSession session = details.VirtualPrinterSession();
        session.VirtualPrinterDataAvailable({ this, &VirtualPrinterTask::OnVirtualPrinterDataAvailable });

        // Kept for parity with the doc sample / potential future use (e.g.
        // reading PrinterUri to distinguish multiple registered queues);
        // this scaffold only ever registers one queue so it's unused today.
        m_printDevice = session.Printer();

        // All event handlers must be registered before Start() per the
        // docs above — do not reorder this.
        session.Start();
    }

    fire_and_forget VirtualPrinterTask::OnVirtualPrinterDataAvailable(
        PrintWorkflowVirtualPrinterSession const& /*sender*/,
        PrintWorkflowVirtualPrinterDataAvailableEventArgs const& args)
    {
        auto lifetime = get_strong();
        PrintWorkflowSubmittedStatus jobStatus = PrintWorkflowSubmittedStatus::Failed;

        try
        {
            PrintWorkflowPdlSourceContent sourceContent = args.SourceContent();
            if (sourceContent.ContentType() != L"application/oxps")
            {
                throw hresult_invalid_argument(L"Unexpected PDL content type; expected application/oxps (PreferredInputFormat in AppxManifest.xml).");
            }

            StorageFolder outputFolder = co_await StorageFolder::GetFolderFromPathAsync(kPhase1OutputDir);
            // TODO(phase-1): GetFolderFromPathAsync throws if kPhase1OutputDir
            // doesn't already exist — this scaffold does not create it.
            // Either pre-create the folder as part of MSIX install, or
            // switch to CreateFolderAsync(..., CreationCollisionOption::OpenIfExists).
            // Left as-is deliberately: unverified on a real machine, don't
            // want to guess at the "right" fix blind.

            hstring fileName = to_hstring(winrt::guid()) + L".pdf";
            StorageFile targetFile = co_await outputFolder.CreateFileAsync(fileName, CreationCollisionOption::ReplaceExisting);

            IRandomAccessStream outputStream = co_await targetFile.OpenAsync(FileAccessMode::ReadWrite);
            PrintWorkflowPdlConverter converter = args.GetPdlConverter(PrintWorkflowPdlConversionType::XpsToPdf);

            co_await converter.ConvertPdlAsync(
                args.GetJobPrintTicket(),
                sourceContent.GetInputStream(),
                outputStream.GetOutputStreamAt(0));

            jobStatus = PrintWorkflowSubmittedStatus::Succeeded;

            // TODO(phase-2): launch the real app with targetFile.Path() here,
            // e.g. via ShellExecute/CreateProcess of Stirling-PDF.exe passing
            // the path as argv[1] — lib.rs's parse_launch_files() +
            // tauri_plugin_single_instance already handle "open this file,
            // forwarding to the running window if one exists" on the Rust
            // side, so nothing new is needed there. Left out of Phase 1
            // because it can't be tested against a real Stirling-PDF.exe
            // from this scaffold in isolation.
        }
        catch (hresult_error const&)
        {
            // TODO(phase-1): log this somewhere inspectable (ETW trace,
            // OutputDebugString, or a file under kPhase1OutputDir) — right
            // now a failure is silent beyond jobStatus staying Failed.
            jobStatus = PrintWorkflowSubmittedStatus::Failed;
        }

        args.CompleteJob(jobStatus);
        m_taskDeferral.Complete();
    }
}
