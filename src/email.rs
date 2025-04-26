use lettre::{
    message::{header::ContentType, Attachment, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    Message, SmtpTransport, Transport,
};
use serde::Deserialize;
use std::{env, fs};

#[derive(Debug, Deserialize)]
pub struct EmailMessage {
    pub from: String,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachment: Option<String>,
}

fn create_attachment(path: Option<String>) -> SinglePart {
    match path {
        Some(attachment_path) => {
            let filebody =
                fs::read(attachment_path).expect("Failed to read attachment {attachment_path}");
            let content_type = ContentType::parse("image/png").unwrap();
            let attachment =
                Attachment::new("ExchangeRate".to_string()).body(filebody, content_type);
            attachment
        }
        None => SinglePart::plain("No plot found...".to_string()),
    }
}

pub fn send_email(message: EmailMessage, debug: bool) {
    let mut builder = Message::builder().from(message.from.parse().unwrap());

    if let Some(to) = message.to {
        builder = builder.to(to.parse().unwrap());
    }
    if let Some(cc) = message.cc {
        builder = builder.cc(cc.parse().unwrap());
    }
    for bcc in message.bcc.iter() {
        builder = builder.bcc(bcc.parse().unwrap());
    }

    let email = builder
        .subject(message.subject)
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(message.body))
                .singlepart(create_attachment(message.attachment)),
        )
        .unwrap();

    let password = env::var("EMAIL_PASS").expect("EMAIL_PASS not found");
    let creds = Credentials::new(message.from, password);

    let mailer = SmtpTransport::relay("smtp.gmail.com")
        .unwrap()
        .credentials(creds)
        .build();

    if debug {
        println!("In Debug: Would send email");
    } else {
        match mailer.send(&email) {
            Ok(_) => println!("Email send successfully."),
            Err(e) => eprintln!("Failed to send email: {}", e),
        }
    }
}
