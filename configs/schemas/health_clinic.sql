-- SQL DDL Frontend Example for Samyama Synthetic Data Compiler
-- Equivalent to configs/schemas/health_clinic.graphql

CREATE TYPE Specialization AS ENUM (
    'CARDIOLOGY',
    'DERMATOLOGY',
    'NEUROLOGY',
    'ONCOLOGY',
    'PEDIATRICS',
    'ORTHOPEDICS',
    'RADIOLOGY'
);

CREATE TYPE AppointmentStatus AS ENUM (
    'SCHEDULED',
    'COMPLETED',
    'CANCELED',
    'NO_SHOW'
);

-- @count(50) @pk(kind: "prefix", prefix: "doc_")
CREATE TABLE Doctor (
    id UUID PRIMARY KEY,
    first_name VARCHAR(64) NOT NULL,
    last_name VARCHAR(64) NOT NULL,
    email VARCHAR(128) NOT NULL,
    specialization Specialization NOT NULL
);

-- @count(200) @pk(kind: "prefix", prefix: "pat_")
CREATE TABLE Patient (
    id UUID PRIMARY KEY,
    first_name VARCHAR(64) NOT NULL,
    last_name VARCHAR(64) NOT NULL,
    email VARCHAR(128) NOT NULL,
    date_of_birth DATE NOT NULL
);

-- @count(500) @pk(kind: "prefix", prefix: "apt_")
CREATE TABLE Appointment (
    id UUID PRIMARY KEY,
    patient_id UUID NOT NULL REFERENCES Patient(id), -- @fk(dist: "uniform", edge: "FOR_PATIENT")
    doctor_id UUID NOT NULL REFERENCES Doctor(id), -- @fk(dist: "zipf", s: 1.25, edge: "WITH_DOCTOR")
    appointment_date DATE NOT NULL, -- @after(field: "patient_id.date_of_birth", min_days: 7300, max_days: 25000)
    appointment_time TIME NOT NULL,
    status AppointmentStatus NOT NULL
);

-- @count(250) @pk(kind: "prefix", prefix: "clm_")
CREATE TABLE Claim (
    id UUID PRIMARY KEY,
    appointment_id UUID NOT NULL REFERENCES Appointment(id), -- @fk(dist: "uniform", edge: "BILLS_APPOINTMENT")
    claim_date DATE NOT NULL, -- @after(field: "appointment_id.appointment_date", min_days: 1, max_days: 30)
    billed_amount DECIMAL(10, 2) NOT NULL CHECK (billed_amount BETWEEN 50 AND 5000),
    claim_status VARCHAR(32) NOT NULL CHECK (claim_status IN ('SUBMITTED', 'APPROVED', 'DENIED', 'PAID'))
);
