//! Single-instance via named mutex (plan §3). Instance kedua mengaktifkan
//! jendela instance pertama lewat pipe (`app.activate`), lalu keluar.

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

pub enum Acquire {
    /// Instance pertama; pegang handle mutex selama proses hidup.
    First(HANDLE),
    /// Sudah ada instance lain.
    Already,
}

pub fn acquire() -> Acquire {
    unsafe {
        let handle = CreateMutexW(None, true, w!("Local\\adm-single-instance"));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    Acquire::Already
                } else {
                    Acquire::First(h)
                }
            }
            Err(_) => Acquire::Already,
        }
    }
}

/// Minta instance yang sedang berjalan untuk memunculkan jendelanya.
pub fn activate_existing() {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return,
    };
    rt.block_on(async {
        use adm_ipc::{method, Request, PIPE_NAME};
        use tokio::io::BufReader;
        use tokio::net::windows::named_pipe::ClientOptions;

        let Ok(client) = ClientOptions::new().open(PIPE_NAME) else {
            return;
        };
        let mut reader = BufReader::new(client);
        let req = Request::new(1, method::APP_ACTIVATE, None);
        let _ = adm_ipc::write_message(reader.get_mut(), &req).await;
        let _ = adm_ipc::read_message(&mut reader).await;
    });
}
